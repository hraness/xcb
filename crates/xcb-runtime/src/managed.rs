use crate::{
    Error, Result, attachments,
    config::Config,
    digest, judge, kernel, new_id, now_ms, private,
    runner::{Observer, Outcome, Progress},
    store::Store,
    summary,
};
use algal::{
    contract::Manifest, effects::Host, graph::Transports, runtime, store::Store as AlgalStore,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex, MutexGuard,
        mpsc::{Receiver, SyncSender, TryRecvError},
    },
    time::{Duration, Instant},
};
use tokio::{sync::watch, task::JoinSet};
use xcb_core::{
    Id, Provider, bounded_text, label,
    policy::{EffectState, Terminal},
    session::{Attachment, Message, Role, State},
    ui::{ConversationRow, Intent, TaskRow, Update, View},
};

const MAX_CONVERSATIONS: i64 = 4096;
const MAX_TASKS: i64 = 4096;
const MAX_NONTERMINAL_TASKS: i64 = 128;
const MAX_MESSAGES: i64 = 50_000;
const MAX_TOTAL_MESSAGES: i64 = 200_000;
const MAX_PREFERENCES: i64 = 256;
const MAX_ACTIVE: usize = 4;
const MAX_TASK_ATTEMPTS: u32 = 4;
const IDLE_EXIT: Duration = Duration::from_secs(30);
const POLICY: &str = include_str!("../managed-transition.algal.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Queued,
    Running,
    NeedsInput,
    Completed,
    Failed,
    Cancelled,
    Uncertain,
}
impl TaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::NeedsInput => "needs_input",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Uncertain => "uncertain",
        }
    }
    fn terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Uncertain
        )
    }
    fn ui(self) -> State {
        match self {
            Self::Queued | Self::Running => State::Working,
            Self::NeedsInput => State::NeedsAnswer,
            Self::Completed => State::Idle,
            Self::Failed => State::Failed,
            Self::Cancelled => State::Cancelled,
            Self::Uncertain => State::Uncertain,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedConversation {
    pub version: u32,
    pub id: Id,
    pub title: String,
    pub workspace: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}
impl ManagedConversation {
    fn validate(&self) -> Result<()> {
        if self.version != 1
            || !Path::new(&self.workspace).is_absolute()
            || self.updated_at_ms < self.created_at_ms
        {
            return Err(xcb_core::Error::Invalid("managed conversation").into());
        }
        label(&self.title, 160)?;
        bounded_text(&self.workspace, 4096)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedTask {
    pub version: u32,
    pub id: Id,
    pub operation: Id,
    pub source_message: Id,
    pub conversation: Id,
    pub workspace: String,
    pub title: String,
    pub goal: String,
    pub next_prompt: String,
    pub attachments: Vec<Attachment>,
    pub session: Option<Id>,
    pub route: Option<String>,
    pub state: TaskState,
    pub detail: String,
    pub attempts: u32,
    pub max_attempts: u32,
    pub message_count_before: usize,
    pub cancel_requested: bool,
    pub last_output: Option<String>,
    pub policy_digest: String,
    pub last_receipt: String,
    pub revision: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}
impl ManagedTask {
    fn validate(&self) -> Result<()> {
        if self.version != 1
            || self.max_attempts == 0
            || self.max_attempts > 32
            || self.attempts > self.max_attempts
            || self.message_count_before > 10_000
            || self.revision == 0
            || self.updated_at_ms < self.created_at_ms
            || !Path::new(&self.workspace).is_absolute()
            || !self.policy_digest.starts_with("sha256:")
            || !self.last_receipt.starts_with("sha256:")
        {
            return Err(xcb_core::Error::Invalid("managed task").into());
        }
        label(&self.title, 160)?;
        bounded_text(&self.workspace, 4096)?;
        bounded_text(&self.goal, xcb_core::MAX_TEXT_BYTES)?;
        bounded_text(&self.next_prompt, xcb_core::MAX_TEXT_BYTES)?;
        bounded_text(&self.detail, 4096)?;
        if self.attachments.len() > 8 {
            return Err(xcb_core::Error::Limit("managed attachments").into());
        }
        for attachment in &self.attachments {
            attachment.validate()?;
        }
        if let Some(output) = &self.last_output {
            bounded_text(output, xcb_core::MAX_TEXT_BYTES)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preference {
    pub version: u32,
    pub id: Id,
    pub scope: String,
    pub text: String,
    pub source_message: Id,
    pub created_at_ms: u64,
}
impl Preference {
    fn validate(&self) -> Result<()> {
        if self.version != 1 || (self.scope != "global" && !Path::new(&self.scope).is_absolute()) {
            return Err(xcb_core::Error::Invalid("managed preference").into());
        }
        bounded_text(&self.text, 4096)?;
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct RouteObservation<'a> {
    version: u32,
    task: &'a Id,
    scope: &'a str,
    provider: Provider,
    outcome: &'static str,
    task_receipt: &'a str,
}

pub struct ManagedStore {
    root: PathBuf,
    connection: Mutex<Connection>,
}

fn sql(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| xcb_core::Error::Invalid("database integer").into())
}
fn decode<T: DeserializeOwned>(text: &str) -> Result<T> {
    Ok(serde_json::from_str(text)?)
}
fn task_from(tx: &Transaction<'_>, id: &Id) -> Result<Option<ManagedTask>> {
    let row: Option<(String, String)> = tx
        .query_row(
            "SELECT payload,conversation FROM tasks WHERE id=?1",
            [id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    row.map(|(payload, conversation)| {
        let task: ManagedTask = decode(&payload)?;
        task.validate()?;
        if task.conversation.as_str() != conversation {
            return Err(Error::Conflict("managed task conversation mismatch"));
        }
        Ok(task)
    })
    .transpose()
}

impl ManagedStore {
    pub fn open(root: &Path) -> Result<Self> {
        let root = private::directory(&root.join("managed"))?;
        let path = root.join("managed.sqlite");
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                private::open_file(&path, 4 * 1024 * 1024 * 1024)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                private::create(&path, &[])?
            }
            Err(error) => return Err(error.into()),
        }
        let mut connection = Connection::open(&path)?;
        connection.busy_timeout(Duration::from_secs(15))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        let mode: String = connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            connection.pragma_update(None, "journal_mode", "WAL")?;
        }
        connection.pragma_update(None, "synchronous", "FULL")?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > 1 {
            return Err(Error::Unavailable(
                "managed state was written by a newer xcb",
            ));
        }
        if version == 0 {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(
                "CREATE TABLE conversations(id TEXT PRIMARY KEY, updated_at INTEGER NOT NULL, payload TEXT NOT NULL);
                 CREATE TABLE messages(id TEXT PRIMARY KEY, conversation TEXT NOT NULL REFERENCES conversations(id), sequence INTEGER NOT NULL, task TEXT, payload TEXT NOT NULL, at_ms INTEGER NOT NULL, UNIQUE(conversation,sequence));
                 CREATE TABLE tasks(id TEXT PRIMARY KEY, operation TEXT NOT NULL UNIQUE, source_message TEXT NOT NULL UNIQUE, conversation TEXT NOT NULL REFERENCES conversations(id), state TEXT NOT NULL, revision INTEGER NOT NULL, updated_at INTEGER NOT NULL, payload TEXT NOT NULL);
                 CREATE INDEX tasks_state_updated ON tasks(state,updated_at,id);
                 CREATE TABLE receipts(digest TEXT PRIMARY KEY, task TEXT, revision INTEGER NOT NULL, payload TEXT NOT NULL);
                 CREATE TABLE preferences(id TEXT PRIMARY KEY, scope TEXT NOT NULL, created_at INTEGER NOT NULL, payload TEXT NOT NULL);
                 CREATE TABLE route_stats(scope TEXT NOT NULL, provider TEXT NOT NULL, completed INTEGER NOT NULL, failed INTEGER NOT NULL, PRIMARY KEY(scope,provider));
                 PRAGMA user_version=1;",
            )?;
            tx.commit()?;
        }
        Ok(Self {
            root,
            connection: Mutex::new(connection),
        })
    }
    fn db(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| Error::Conflict("managed database lock poisoned"))
    }
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub async fn create_conversation(&self, workspace: &Path) -> Result<ManagedConversation> {
        let workspace = workspace.to_str().ok_or(Error::PrivateState)?.to_owned();
        let now = now_ms();
        let id = new_id("c");
        let project = Path::new(&workspace)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("workspace");
        let conversation = ManagedConversation {
            version: 1,
            id: id.clone(),
            title: format!("{project} · {}", &id.as_str()[..10.min(id.as_str().len())]),
            workspace,
            created_at_ms: now,
            updated_at_ms: now,
        };
        conversation.validate()?;
        let (_, receipt, receipt_json) = Self::algal_receipt(&conversation).await?;
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count: i64 =
            tx.query_row("SELECT count(*) FROM conversations", [], |row| row.get(0))?;
        if count >= MAX_CONVERSATIONS {
            return Err(xcb_core::Error::Limit("managed conversations").into());
        }
        tx.execute(
            "INSERT INTO conversations(id,updated_at,payload) VALUES(?1,?2,?3)",
            params![
                conversation.id.as_str(),
                sql(now)?,
                serde_json::to_string(&conversation)?
            ],
        )?;
        tx.execute(
            "INSERT INTO receipts(digest,task,revision,payload) VALUES(?1,NULL,?2,?3)",
            params![receipt, sql(now)?, receipt_json],
        )?;
        tx.commit()?;
        Ok(conversation)
    }

    pub fn conversation(&self, id: &Id) -> Result<Option<ManagedConversation>> {
        let db = self.db()?;
        let row: Option<(String, i64)> = db
            .query_row(
                "SELECT payload,updated_at FROM conversations WHERE id=?1",
                [id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(payload, updated)| {
            let mut conversation: ManagedConversation = decode(&payload)?;
            conversation.updated_at_ms = u64::try_from(updated)
                .map_err(|_| xcb_core::Error::Invalid("conversation timestamp"))?;
            conversation.validate()?;
            Ok(conversation)
        })
        .transpose()
    }

    pub fn conversations(&self, limit: usize) -> Result<Vec<ManagedConversation>> {
        if !(1..=256).contains(&limit) {
            return Err(xcb_core::Error::Invalid("managed conversation page").into());
        }
        let db = self.db()?;
        let mut query = db.prepare(
            "SELECT payload,updated_at FROM conversations ORDER BY updated_at DESC,id LIMIT ?1",
        )?;
        let rows = query.query_map([limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut values = Vec::new();
        for row in rows {
            let (payload, updated) = row?;
            let mut conversation: ManagedConversation = decode(&payload)?;
            conversation.updated_at_ms = u64::try_from(updated)
                .map_err(|_| xcb_core::Error::Invalid("conversation timestamp"))?;
            conversation.validate()?;
            values.push(conversation);
        }
        Ok(values)
    }

    pub fn messages(&self, conversation: &Id, limit: usize) -> Result<Vec<Message>> {
        if !(1..=512).contains(&limit) {
            return Err(xcb_core::Error::Invalid("managed message page").into());
        }
        let db = self.db()?;
        let mut query = db.prepare("SELECT payload FROM (SELECT sequence,payload FROM messages WHERE conversation=?1 ORDER BY sequence DESC LIMIT ?2) ORDER BY sequence")?;
        let rows = query.query_map(params![conversation.as_str(), limit as i64], |row| {
            row.get::<_, String>(0)
        })?;
        let mut messages = Vec::new();
        let mut bytes = 0usize;
        for row in rows {
            let row = row?;
            bytes = bytes.saturating_add(row.len());
            if bytes > 8 * 1024 * 1024 {
                return Err(xcb_core::Error::Limit("managed transcript").into());
            }
            let message: Message = decode(&row)?;
            message.validate()?;
            messages.push(message);
        }
        Ok(messages)
    }

    pub fn tasks(&self, limit: usize) -> Result<Vec<ManagedTask>> {
        if !(1..=256).contains(&limit) {
            return Err(xcb_core::Error::Invalid("managed task page").into());
        }
        let db = self.db()?;
        let mut query = db.prepare(
            "SELECT payload,conversation FROM tasks ORDER BY updated_at DESC,id LIMIT ?1",
        )?;
        let rows = query.query_map([limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut tasks = Vec::new();
        for row in rows {
            let (payload, conversation) = row?;
            let task: ManagedTask = decode(&payload)?;
            task.validate()?;
            if task.conversation.as_str() != conversation {
                return Err(Error::Conflict("managed task conversation mismatch"));
            }
            tasks.push(task);
        }
        Ok(tasks)
    }
    fn active_tasks(&self, limit: usize) -> Result<Vec<ManagedTask>> {
        if !(1..=256).contains(&limit) {
            return Err(xcb_core::Error::Invalid("managed active task page").into());
        }
        let db = self.db()?;
        let mut query = db.prepare("SELECT payload,conversation FROM tasks WHERE state IN ('queued','running','needs_input') ORDER BY updated_at DESC,id LIMIT ?1")?;
        let rows = query.query_map([limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut tasks = Vec::new();
        for row in rows {
            let (payload, conversation) = row?;
            let task: ManagedTask = decode(&payload)?;
            task.validate()?;
            if task.conversation.as_str() != conversation || task.state.terminal() {
                return Err(Error::Conflict("managed active task mismatch"));
            }
            tasks.push(task);
        }
        Ok(tasks)
    }
    pub fn task(&self, id: &Id) -> Result<Option<ManagedTask>> {
        let db = self.db()?;
        let row: Option<(String, String)> = db
            .query_row(
                "SELECT payload,conversation FROM tasks WHERE id=?1",
                [id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(payload, conversation)| {
            let task: ManagedTask = decode(&payload)?;
            task.validate()?;
            if task.conversation.as_str() != conversation {
                return Err(Error::Conflict("managed task conversation mismatch"));
            }
            Ok(task)
        })
        .transpose()
    }
    pub fn preferences(&self, workspace: &Path) -> Result<Vec<Preference>> {
        let scope = workspace.to_string_lossy();
        let db = self.db()?;
        let mut query = db.prepare("SELECT payload FROM preferences WHERE scope='global' OR scope=?1 ORDER BY created_at,id LIMIT ?2")?;
        let rows = query.query_map(params![scope.as_ref(), MAX_PREFERENCES], |row| {
            row.get::<_, String>(0)
        })?;
        let mut values = Vec::new();
        for row in rows {
            let value: Preference = decode(&row?)?;
            value.validate()?;
            values.push(value);
        }
        Ok(values)
    }

    fn learned_route(&self, workspace: &Path) -> Result<Option<Provider>> {
        let scope = workspace.to_string_lossy();
        let db = self.db()?;
        let mut query = db.prepare(
            "SELECT provider,completed,failed FROM route_stats WHERE scope=?1 ORDER BY provider",
        )?;
        let rows = query.query_map([scope.as_ref()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut eligible = Vec::new();
        for row in rows {
            let (provider, completed, failed) = row?;
            if completed >= 2
                && completed.saturating_mul(3) >= completed.saturating_add(failed).saturating_mul(2)
            {
                eligible.push((
                    provider.parse::<Provider>()?,
                    completed.saturating_sub(failed),
                ));
            }
        }
        eligible.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        match eligible.as_slice() {
            [(provider, _)] => Ok(Some(*provider)),
            [(provider, score), (_, next), ..] if score > next => Ok(Some(*provider)),
            _ => Ok(None),
        }
    }

    async fn record_route_observation(&self, task: &ManagedTask) -> Result<()> {
        let outcome = match task.state {
            TaskState::Completed => "completed",
            TaskState::Failed => "failed",
            _ => return Ok(()),
        };
        let Some(provider) = task
            .route
            .as_deref()
            .and_then(|route| route.split('/').next())
            .and_then(|provider| provider.parse::<Provider>().ok())
        else {
            return Ok(());
        };
        let observation = RouteObservation {
            version: 1,
            task: &task.id,
            scope: &task.workspace,
            provider,
            outcome,
            task_receipt: &task.last_receipt,
        };
        let (_, receipt, receipt_json) = Self::algal_receipt(&observation).await?;
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let recorded: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM receipts WHERE digest=?1)",
            [&receipt],
            |row| row.get(0),
        )?;
        if recorded {
            return Ok(());
        }
        tx.execute(
            "INSERT INTO route_stats(scope,provider,completed,failed) VALUES(?1,?2,?3,?4) ON CONFLICT(scope,provider) DO UPDATE SET completed=completed+excluded.completed,failed=failed+excluded.failed",
            params![task.workspace, provider.as_str(), i64::from(outcome == "completed"), i64::from(outcome != "completed")],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO receipts(digest,task,revision,payload) VALUES(?1,?2,?3,?4)",
            params![receipt, task.id.as_str(), sql(task.revision)?, receipt_json],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn existing_message(&self, id: &Id) -> Result<bool> {
        Ok(self.db()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE id=?1)",
            [id.as_str()],
            |row| row.get(0),
        )?)
    }

    fn append_message_tx(
        tx: &Transaction<'_>,
        message: &Message,
        conversation: &Id,
        task: Option<&Id>,
    ) -> Result<()> {
        message.validate()?;
        let total: i64 = tx.query_row("SELECT count(*) FROM messages", [], |row| row.get(0))?;
        let count: i64 = tx.query_row(
            "SELECT count(*) FROM messages WHERE conversation=?1",
            [conversation.as_str()],
            |row| row.get(0),
        )?;
        if total >= MAX_TOTAL_MESSAGES || count >= MAX_MESSAGES {
            return Err(xcb_core::Error::Limit("managed messages").into());
        }
        let sequence: i64 = tx.query_row(
            "SELECT COALESCE(max(sequence),0)+1 FROM messages WHERE conversation=?1",
            [conversation.as_str()],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO messages(id,conversation,sequence,task,payload,at_ms) VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                message.id.as_str(),
                conversation.as_str(),
                sequence,
                task.map(Id::as_str),
                serde_json::to_string(message)?,
                sql(message.at_ms)?
            ],
        )?;
        tx.execute(
            "UPDATE conversations SET updated_at=?1 WHERE id=?2",
            params![sql(message.at_ms)?, conversation.as_str()],
        )?;
        Ok(())
    }

    fn assistant(text: impl Into<String>, task: Option<&Id>, revision: u64) -> Message {
        let text = text.into();
        let id = task.map_or_else(
            || new_id("m"),
            |task| {
                let key = format!("xcb-managed-message-v1\0{task}\0{revision}\0{text}");
                Id::new(format!("m_{}", digest(key))).expect("digest id")
            },
        );
        Message {
            id,
            role: Role::Assistant,
            text,
            at_ms: now_ms(),
            attachments: vec![],
            provenance: None,
        }
    }

    async fn algal_receipt<T: Serialize>(record: &T) -> Result<(String, String, String)> {
        let source: Value = serde_json::from_str(POLICY)?;
        let manifest = Manifest::parse(&source)
            .map_err(|_| Error::Unavailable("Algal transition policy rejected"))?;
        let policy_digest = manifest
            .digest()
            .map_err(|_| Error::Unavailable("Algal transition policy rejected"))?;
        let input = serde_json::to_value(record)?;
        let mut store = AlgalStore::default();
        let mut host = Host::default();
        let receipt = runtime::run(
            manifest.clone(),
            json!({"src":{"record":input}}),
            &mut store,
            &mut host,
            &Transports::new(),
            None,
        )
        .await
        .map_err(|_| Error::Unavailable("Algal transition failed"))?;
        if receipt["outcome"] != "complete"
            || runtime::outputs(&manifest, &receipt)
                .map_err(|_| Error::Unavailable("Algal transition output rejected"))?["record"]
                != input
        {
            return Err(Error::Unavailable("Algal transition output rejected"));
        }
        let receipt_digest = receipt["digest"]
            .as_str()
            .ok_or(Error::Unavailable("Algal transition receipt missing"))?
            .to_owned();
        Ok((
            policy_digest,
            receipt_digest,
            serde_json::to_string(&receipt)?,
        ))
    }

    async fn transition(
        &self,
        expected: &ManagedTask,
        mut next: ManagedTask,
        message: Option<Message>,
    ) -> Result<ManagedTask> {
        next.validate()?;
        if next.id != expected.id
            || next.operation != expected.operation
            || next.source_message != expected.source_message
            || next.conversation != expected.conversation
            || next.workspace != expected.workspace
            || next.revision != expected.revision + 1
        {
            return Err(Error::Conflict("managed task transition changed identity"));
        }
        let (policy, receipt, receipt_json) = Self::algal_receipt(&next).await?;
        if policy != expected.policy_digest || next.policy_digest != policy {
            return Err(Error::Conflict("managed task policy changed"));
        }
        next.last_receipt = receipt.clone();
        next.validate()?;
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current =
            task_from(&tx, &expected.id)?.ok_or(Error::Unavailable("managed task not found"))?;
        if current.revision != expected.revision
            || serde_json::to_string(&current)? != serde_json::to_string(expected)?
        {
            return Err(Error::Conflict("managed task revision changed"));
        }
        if tx.execute("UPDATE tasks SET state=?1,revision=?2,updated_at=?3,payload=?4 WHERE id=?5 AND revision=?6", params![next.state.as_str(), sql(next.revision)?, sql(next.updated_at_ms)?, serde_json::to_string(&next)?, next.id.as_str(), sql(expected.revision)?])? != 1 {
            return Err(Error::Conflict("managed task revision changed"));
        }
        tx.execute(
            "INSERT INTO receipts(digest,task,revision,payload) VALUES(?1,?2,?3,?4)",
            params![receipt, next.id.as_str(), sql(next.revision)?, receipt_json],
        )?;
        if let Some(message) = &message {
            Self::append_message_tx(&tx, message, &next.conversation, Some(&next.id))?;
        }
        tx.commit()?;
        Ok(next)
    }

    async fn create_task(
        &self,
        conversation: &Id,
        id: Id,
        text: String,
        attachments: Vec<Attachment>,
        workspace: &Path,
    ) -> Result<ManagedTask> {
        bounded_text(&text, xcb_core::MAX_TEXT_BYTES)?;
        if attachments.len() > 8 {
            return Err(xcb_core::Error::Limit("attachments").into());
        }
        let selected_route = route_hint(&text).or(self.learned_route(workspace)?);
        let workspace = workspace.to_str().ok_or(Error::PrivateState)?.to_owned();
        let current = self
            .conversation(conversation)?
            .ok_or(Error::Unavailable("managed conversation not found"))?;
        if current.workspace != workspace {
            return Err(Error::Conflict("managed conversation workspace changed"));
        }
        if !Path::new(&workspace).is_dir() {
            return Err(Error::Unavailable(
                "managed conversation workspace is unavailable",
            ));
        }
        let task_id = Id::new(format!(
            "t_{}",
            digest(format!("xcb-task-v1\0{conversation}\0{id}\0{workspace}"))
        ))?;
        let operation = Id::new(format!(
            "op_{}",
            digest(format!(
                "xcb-operation-v1\0{conversation}\0{id}\0{workspace}\0{text}"
            ))
        ))?;
        let now = now_ms();
        let title = text
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("Untitled task")
            .trim();
        let title: String = title.chars().take(120).collect();
        let source: Value = serde_json::from_str(POLICY)?;
        let policy = Manifest::parse(&source)
            .map_err(|_| Error::Unavailable("Algal transition policy rejected"))?
            .digest()
            .map_err(|_| Error::Unavailable("Algal transition policy rejected"))?;
        let mut task = ManagedTask {
            version: 1,
            id: task_id.clone(),
            operation,
            source_message: id.clone(),
            conversation: conversation.clone(),
            workspace,
            title,
            goal: text.clone(),
            next_prompt: text.clone(),
            attachments: attachments.clone(),
            session: None,
            route: selected_route.map(|provider| provider.to_string()),
            state: TaskState::Queued,
            detail: "waiting for an eligible worker".into(),
            attempts: 0,
            max_attempts: MAX_TASK_ATTEMPTS,
            message_count_before: 0,
            cancel_requested: false,
            last_output: None,
            policy_digest: policy,
            last_receipt: "sha256:pending".into(),
            revision: 1,
            created_at_ms: now,
            updated_at_ms: now,
        };
        let (_, receipt, receipt_json) = Self::algal_receipt(&task).await?;
        task.last_receipt = receipt.clone();
        task.validate()?;
        let user = Message {
            id,
            role: Role::User,
            text,
            at_ms: now,
            attachments,
            provenance: None,
        };
        let ack = Self::assistant(
            format!(
                "Started **{}**. I’ll keep it moving in the background and bring back results or a specific question.",
                task.title
            ),
            Some(&task.id),
            task.revision,
        );
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count: i64 = tx.query_row("SELECT count(*) FROM tasks", [], |row| row.get(0))?;
        let active: i64 = tx.query_row(
            "SELECT count(*) FROM tasks WHERE state IN ('queued','running','needs_input')",
            [],
            |row| row.get(0),
        )?;
        if count >= MAX_TASKS {
            return Err(xcb_core::Error::Limit("managed tasks").into());
        }
        if active >= MAX_NONTERMINAL_TASKS {
            return Err(xcb_core::Error::Limit("active managed tasks").into());
        }
        Self::append_message_tx(&tx, &user, conversation, Some(&task.id))?;
        tx.execute("INSERT INTO tasks(id,operation,source_message,conversation,state,revision,updated_at,payload) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)", params![task.id.as_str(), task.operation.as_str(), task.source_message.as_str(), conversation.as_str(), task.state.as_str(), sql(task.revision)?, sql(task.updated_at_ms)?, serde_json::to_string(&task)?])?;
        tx.execute(
            "INSERT INTO receipts(digest,task,revision,payload) VALUES(?1,?2,?3,?4)",
            params![receipt, task.id.as_str(), sql(task.revision)?, receipt_json],
        )?;
        Self::append_message_tx(&tx, &ack, conversation, Some(&task.id))?;
        tx.commit()?;
        Ok(task)
    }

    fn record_pair(&self, conversation: &Id, id: Id, text: String, answer: String) -> Result<()> {
        let now = now_ms();
        let user = Message {
            id,
            role: Role::User,
            text,
            at_ms: now,
            attachments: vec![],
            provenance: None,
        };
        let assistant = Self::assistant(answer, None, now);
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::append_message_tx(&tx, &user, conversation, None)?;
        Self::append_message_tx(&tx, &assistant, conversation, None)?;
        tx.commit()?;
        Ok(())
    }

    async fn reply(
        &self,
        task: &ManagedTask,
        conversation: &Id,
        id: Id,
        text: String,
        attachments: Vec<Attachment>,
    ) -> Result<ManagedTask> {
        let now = now_ms();
        let mut next = task.clone();
        next.state = TaskState::Queued;
        next.next_prompt = text.clone();
        next.attachments = attachments.clone();
        next.detail = "your answer is queued for the worker".into();
        next.cancel_requested = false;
        next.revision += 1;
        next.updated_at_ms = now;
        let local = &task.conversation == conversation;
        let message = if local {
            Message {
                id: id.clone(),
                role: Role::User,
                text: text.clone(),
                at_ms: now,
                attachments,
                provenance: None,
            }
        } else {
            Self::assistant(
                format!("Input supplied from another chat: {text}"),
                Some(&task.id),
                next.revision,
            )
        };
        let updated = self.transition(task, next, Some(message)).await?;
        if !local {
            self.record_pair(
                conversation,
                id,
                text,
                format!("Sent your answer to **{}**.", task.title),
            )?;
        }
        Ok(updated)
    }

    async fn remember(
        &self,
        conversation: &Id,
        id: Id,
        text: String,
        workspace: &Path,
    ) -> Result<()> {
        let clean = text
            .split_once(':')
            .map(|(_, value)| value.trim())
            .unwrap_or("")
            .to_owned();
        bounded_text(&clean, 4096)?;
        if clean.is_empty() {
            return Err(xcb_core::Error::Invalid("preference").into());
        }
        let scope = workspace.to_str().ok_or(Error::PrivateState)?.to_owned();
        let preference = Preference {
            version: 1,
            id: Id::new(format!(
                "pref_{}",
                digest(format!(
                    "xcb-preference-v1\0{scope}\0{}",
                    clean.to_lowercase()
                ))
            ))?,
            scope,
            text: clean.clone(),
            source_message: id.clone(),
            created_at_ms: now_ms(),
        };
        preference.validate()?;
        let (_, receipt, receipt_json) = Self::algal_receipt(&preference).await?;
        let user = Message {
            id,
            role: Role::User,
            text,
            at_ms: now_ms(),
            attachments: vec![],
            provenance: None,
        };
        let ack = Self::assistant(
            format!("Remembered for this workspace: {clean}"),
            None,
            preference.created_at_ms,
        );
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count: i64 = tx.query_row("SELECT count(*) FROM preferences", [], |row| row.get(0))?;
        if count >= MAX_PREFERENCES {
            return Err(xcb_core::Error::Limit("managed preferences").into());
        }
        Self::append_message_tx(&tx, &user, conversation, None)?;
        tx.execute(
            "INSERT OR IGNORE INTO preferences(id,scope,created_at,payload) VALUES(?1,?2,?3,?4)",
            params![
                preference.id.as_str(),
                preference.scope,
                sql(preference.created_at_ms)?,
                serde_json::to_string(&preference)?
            ],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO receipts(digest,task,revision,payload) VALUES(?1,NULL,?2,?3)",
            params![receipt, sql(preference.created_at_ms)?, receipt_json],
        )?;
        Self::append_message_tx(&tx, &ack, conversation, None)?;
        tx.commit()?;
        Ok(())
    }

    pub async fn submit(
        &self,
        conversation: &Id,
        id: Id,
        text: String,
        attachments: Vec<Attachment>,
        workspace: &Path,
    ) -> Result<()> {
        if self.existing_message(&id)? {
            return Ok(());
        }
        let current = self
            .conversation(conversation)?
            .ok_or(Error::Unavailable("managed conversation not found"))?;
        if current.workspace != workspace.to_string_lossy() {
            return Err(Error::Conflict("managed conversation workspace changed"));
        }
        let trimmed = text.trim();
        if attachments.is_empty() && memory_question(trimmed) {
            return self.record_pair(conversation, id, text, self.memory_text(workspace)?);
        }
        if attachments.is_empty() && status_question(trimmed) {
            return self.record_pair(conversation, id, text, self.status_text()?);
        }
        if attachments.is_empty() && trimmed.to_ascii_lowercase().starts_with("remember:") {
            return self.remember(conversation, id, text, workspace).await;
        }
        let tasks = self.active_tasks(128)?;
        if attachments.is_empty() && cancel_request(trimmed) {
            let active: Vec<_> = tasks.iter().filter(|task| !task.state.terminal()).collect();
            let lower = trimmed.to_ascii_lowercase();
            let needle = lower
                .strip_prefix("cancel")
                .or_else(|| lower.strip_prefix("stop"))
                .unwrap_or("")
                .trim();
            let matches: Vec<_> = if needle.is_empty() {
                active
                    .iter()
                    .copied()
                    .filter(|task| &task.conversation == conversation)
                    .collect()
            } else {
                active
                    .iter()
                    .copied()
                    .filter(|task| {
                        task.id.as_str().to_ascii_lowercase().starts_with(needle)
                            || task.title.to_ascii_lowercase().contains(needle)
                    })
                    .collect()
            };
            let task = match matches.as_slice() {
                [task] => (*task).clone(),
                [] => return self.record_pair(conversation, id, text, "I couldn’t identify an active task to cancel. Use `/tasks`, then say `cancel <task id or title>`.".into()),
                _ => return self.record_pair(conversation, id, text, "More than one task is active. Use `/tasks`, then say `cancel <task id or title>` so I don’t stop the wrong work.".into()),
            };
            let mut task = task;
            for _ in 0..2 {
                let now = now_ms();
                let mut next = task.clone();
                next.cancel_requested = true;
                next.detail = "cancellation requested; waiting for confirmed settlement".into();
                next.revision += 1;
                next.updated_at_ms = now;
                let local = &task.conversation == conversation;
                let user = local.then(|| Message {
                    id: id.clone(),
                    role: Role::User,
                    text: text.clone(),
                    at_ms: now,
                    attachments: vec![],
                    provenance: None,
                });
                match self.transition(&task, next, user).await {
                    Ok(_) if local => return Ok(()),
                    Ok(_) => {
                        return self.record_pair(
                            conversation,
                            id,
                            text,
                            format!("Cancellation requested for **{}**.", task.title),
                        );
                    }
                    Err(Error::Conflict(_)) => {
                        task = self
                            .task(&task.id)?
                            .ok_or(Error::Unavailable("managed task not found"))?;
                        if task.state.terminal() {
                            return self.record_pair(
                                conversation,
                                id,
                                text,
                                format!("**{}** already stopped: {}.", task.title, task.detail),
                            );
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
            return Err(Error::Conflict(
                "managed task changed while requesting cancellation",
            ));
        }
        let pending: Vec<_> = tasks
            .iter()
            .filter(|task| task.state == TaskState::NeedsInput)
            .collect();
        let lower = trimmed.to_ascii_lowercase();
        let force_new = lower.starts_with("new task:");
        if !force_new && !pending.is_empty() {
            let local: Vec<_> = pending
                .iter()
                .copied()
                .filter(|task| &task.conversation == conversation)
                .collect();
            let addressed: Vec<_> = pending
                .iter()
                .copied()
                .filter(|task| {
                    lower.contains(&task.id.as_str().to_ascii_lowercase())
                        || lower.contains(&task.title.to_ascii_lowercase())
                })
                .collect();
            let target = if let [task] = addressed.as_slice() {
                Some(*task)
            } else if addressed.is_empty()
                && let [task] = local.as_slice()
            {
                Some(*task)
            } else {
                None
            };
            if let Some(task) = target {
                return match self.reply(task, conversation, id.clone(), text.clone(), attachments).await {
                    Ok(_) => Ok(()),
                    Err(Error::Conflict(_)) => self.record_pair(
                        conversation,
                        id,
                        text,
                        "That task changed before your answer was delivered. I did not redirect it to another task; check `/tasks` and send it again if still needed.".into(),
                    ),
                    Err(error) => Err(error),
                };
            }
            if !local.is_empty() || !addressed.is_empty() || reply_like(trimmed) {
                return self.record_pair(conversation, id, text, "One or more tasks need input. Include the task id or title so I don’t send your answer to the wrong worker.".into());
            }
        }
        let text = if force_new {
            trimmed
                .split_once(':')
                .map(|(_, text)| text.trim().to_owned())
                .unwrap_or(text)
        } else {
            text
        };
        self.create_task(conversation, id, text, attachments, workspace)
            .await?;
        Ok(())
    }

    pub fn memory_text(&self, workspace: &Path) -> Result<String> {
        let mut lines = Vec::new();
        let preferences = self.preferences(workspace)?;
        if !preferences.is_empty() {
            lines.push("Workspace preferences:".into());
            for preference in preferences.iter().take(16) {
                lines.push(format!("- {}", preference.text));
            }
        }
        let workspace = workspace.to_string_lossy();
        let completed: Vec<_> = self
            .tasks(64)?
            .into_iter()
            .filter(|task| task.workspace == workspace && task.state.terminal())
            .take(8)
            .collect();
        if !completed.is_empty() {
            lines.push("Recent task evidence:".into());
            for task in &completed {
                lines.push(task_status(task));
            }
        }
        if lines.is_empty() {
            lines.push("I don’t have a retained workspace preference or completed task result for this workspace yet.".into());
        }
        Ok(lines.join("\n"))
    }

    pub fn status_text(&self) -> Result<String> {
        let active = self.active_tasks(128)?;
        if active.is_empty() {
            let tasks = self.tasks(32)?;
            let mut lines =
                vec!["Nothing needs you right now. No managed tasks are running.".to_owned()];
            if !tasks.is_empty() {
                lines.push("Recent work:".into());
                for task in tasks.iter().take(5) {
                    lines.push(task_status(task));
                }
            }
            return Ok(lines.join("\n"));
        }
        let mut lines = vec![format!(
            "{} active task{}:",
            active.len(),
            if active.len() == 1 { "" } else { "s" }
        )];
        for task in &active {
            lines.push(task_status(task));
        }
        Ok(lines.join("\n"))
    }

    async fn settle_unstarted_cancel(&self, task: &ManagedTask) -> Result<ManagedTask> {
        let mut next = task.clone();
        next.state = TaskState::Cancelled;
        next.detail = "cancelled before worker dispatch".into();
        next.cancel_requested = false;
        next.next_prompt.clear();
        next.attachments.clear();
        next.revision += 1;
        next.updated_at_ms = now_ms();
        let message = Self::assistant(
            format!("**{}** · cancelled before worker dispatch", next.title),
            Some(&next.id),
            next.revision,
        );
        self.transition(task, next, Some(message)).await
    }

    async fn prepare(
        &self,
        task: &ManagedTask,
        session: Id,
        route: String,
        message_count: usize,
    ) -> Result<ManagedTask> {
        let mut next = task.clone();
        next.session = Some(session);
        next.message_count_before = message_count;
        next.route = Some(route);
        next.state = TaskState::Running;
        next.detail = "worker is running".into();
        next.cancel_requested = false;
        next.revision += 1;
        next.updated_at_ms = now_ms();
        self.transition(task, next, None).await
    }

    async fn finish(&self, store: &Store, id: &Id, result: Result<Outcome>) -> Result<ManagedTask> {
        let task = self
            .task(id)?
            .ok_or(Error::Unavailable("managed task not found"))?;
        let (unsettled, message_count, session_state) = if let Some(session) = &task.session {
            (
                store
                    .unsettled_runs()?
                    .iter()
                    .any(|run| run.session.as_ref() == Some(session)),
                store.message_count(session)?,
                store.session(session)?.map(|session| session.state),
            )
        } else {
            (false, 0, None)
        };
        let dispatch_unstarted = !unsettled && message_count == task.message_count_before;
        let cancellation_settled =
            !unsettled && (dispatch_unstarted || session_state == Some(State::Cancelled));
        let continue_task = match &result {
            Ok(outcome) => tokio::time::timeout(
                Duration::from_secs(5),
                task_should_continue(store, &task, outcome),
            )
            .await
            .ok()
            .and_then(std::result::Result::ok)
            .unwrap_or(false),
            Err(_) => false,
        };
        let mut next = task.clone();
        next.attempts = next.attempts.saturating_add(1);
        next.revision += 1;
        next.updated_at_ms = now_ms();
        let (state, detail, output) = match result {
            Ok(outcome) if continue_task => (
                TaskState::Queued,
                "the supervisor is continuing unfinished work in the same session".into(),
                Some(outcome.text),
            ),
            Ok(outcome) => {
                let state = match outcome.state {
                    State::NeedsAnswer | State::NeedsAction | State::NeedsApproval => {
                        TaskState::NeedsInput
                    }
                    State::Idle
                        if outcome.facts.terminal == Terminal::Completed
                            && outcome.facts.joined
                            && outcome.facts.effects != EffectState::Uncertain =>
                    {
                        TaskState::Completed
                    }
                    State::Cancelled => TaskState::Cancelled,
                    State::Uncertain => TaskState::Uncertain,
                    _ => TaskState::Failed,
                };
                let detail = match state {
                    TaskState::NeedsInput => "the worker needs your input",
                    TaskState::Completed => {
                        "worker finished with settled execution; checks are worker-reported"
                    }
                    TaskState::Cancelled => "worker cancellation settled",
                    TaskState::Uncertain => {
                        "worker settlement is uncertain; no retry will be launched"
                    }
                    _ => "worker stopped without completing the task",
                }
                .to_owned();
                (state, detail, Some(outcome.text))
            }
            Err(error) if task.cancel_requested && cancellation_settled => (
                TaskState::Cancelled,
                "worker cancellation settled".into(),
                Some(error.to_string()),
            ),
            Err(error) if dispatch_unstarted && task.attempts < task.max_attempts => {
                next.attempts = task.attempts;
                (
                    TaskState::Queued,
                    "dispatch did not cross the provider boundary; waiting for an eligible route"
                        .into(),
                    Some(error.to_string()),
                )
            }
            Err(error) => (
                TaskState::Uncertain,
                "worker outcome is uncertain; no automatic retry".into(),
                Some(error.to_string()),
            ),
        };
        next.state = state;
        next.detail = detail;
        next.last_output = output.clone();
        if state == TaskState::Queued && continue_task {
            next.next_prompt = "Continue the original task from the last confirmed checkpoint. Do not repeat completed effects or expand scope. Stop and ask one specific question if input or approval is required.".into();
            next.attachments.clear();
        } else if state != TaskState::Queued {
            next.next_prompt.clear();
            next.attachments.clear();
        }
        let message = if state == TaskState::Queued {
            None
        } else {
            let body = output
                .as_deref()
                .unwrap_or("No response text was retained.");
            Some(Self::assistant(
                format!(
                    "**{}** · {}\n\n{}",
                    next.title,
                    next.detail,
                    xcb_core::display_text(body, xcb_core::MAX_TEXT_BYTES - 512)
                ),
                Some(&next.id),
                next.revision,
            ))
        };
        let finished = self.transition(&task, next, message).await?;
        let _ = self.record_route_observation(&finished).await;
        Ok(finished)
    }

    pub async fn reconcile_startup(&self, store: &Store) -> Result<()> {
        let unsettled = store.unsettled_runs()?;
        for task in self
            .active_tasks(128)?
            .into_iter()
            .filter(|task| task.state == TaskState::Running)
        {
            let Some(session_id) = &task.session else {
                let mut next = task.clone();
                next.state = TaskState::Queued;
                next.detail = "dispatch was not admitted; queued again".into();
                next.revision += 1;
                next.updated_at_ms = now_ms();
                self.transition(&task, next, None).await?;
                continue;
            };
            if unsettled
                .iter()
                .any(|run| run.session.as_ref() == Some(session_id))
            {
                let mut next = task.clone();
                next.state = TaskState::Uncertain;
                next.detail =
                    "supervisor restarted with an unsettled worker; explicit recovery required"
                        .into();
                next.revision += 1;
                next.updated_at_ms = now_ms();
                let message = Self::assistant(
                    format!(
                        "**{}** needs recovery. Its prior worker did not leave conclusive settlement evidence, so I did not retry it.",
                        task.title
                    ),
                    Some(&task.id),
                    next.revision,
                );
                self.transition(&task, next, Some(message)).await?;
                continue;
            }
            let session = store
                .session(session_id)?
                .ok_or(Error::Unavailable("managed worker session missing"))?;
            let total = store.message_count(session_id)?;
            if total == task.message_count_before {
                let mut next = task.clone();
                next.state = TaskState::Queued;
                next.detail = "dispatch stopped before provider admission; queued again".into();
                next.revision += 1;
                next.updated_at_ms = now_ms();
                self.transition(&task, next, None).await?;
                continue;
            }
            let delta = total.saturating_sub(task.message_count_before);
            let messages = (delta <= 512)
                .then(|| store.messages(session_id, delta.max(1)))
                .transpose()?;
            if let Some(answer) = messages.as_ref().and_then(|messages| {
                messages
                    .iter()
                    .rev()
                    .find(|message| message.role == Role::Assistant)
            }) {
                let outcome = Outcome {
                    text: answer.text.clone(),
                    facts: xcb_core::policy::TurnFacts {
                        terminal: if session.state == State::Cancelled {
                            Terminal::Cancelled
                        } else if session.state == State::Idle {
                            Terminal::Completed
                        } else {
                            Terminal::Failed
                        },
                        joined: true,
                        effects: EffectState::Settled,
                        pending_attention: session.state.attention(),
                        failure: None,
                    },
                    state: session.state,
                };
                self.finish(store, &task.id, Ok(outcome)).await?;
            } else {
                let mut next = task.clone();
                next.state = TaskState::Uncertain;
                next.detail = "the worker crossed the provider boundary without a terminal result; no retry will be launched".into();
                next.revision += 1;
                next.updated_at_ms = now_ms();
                let message = Self::assistant(
                    format!(
                        "**{}** needs recovery. Its worker started but no terminal response was retained, so I did not retry it.",
                        task.title
                    ),
                    Some(&task.id),
                    next.revision,
                );
                self.transition(&task, next, Some(message)).await?;
            }
        }
        Ok(())
    }
}

fn task_status(task: &ManagedTask) -> String {
    let project = Path::new(&task.workspace)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace");
    let mut line = format!(
        "- **{}** · {} · {} · {}",
        task.title,
        task.state.as_str().replace('_', " "),
        project,
        task.detail
    );
    if let Some(output) = &task.last_output {
        let summary = xcb_core::display_text(output.lines().next().unwrap_or(""), 320);
        if !summary.is_empty() {
            line.push_str(&format!("\n  {summary}"));
        }
    }
    line
}

fn memory_question(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("what did we learn")
        || lower.contains("what do you remember")
        || lower.contains("what have you learned")
        || lower.contains("remember about this")
}

fn status_question(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower == "status"
        || lower == "what's running?"
        || lower == "whats running?"
        || lower.contains("what needs me")
        || lower.contains("what is running")
        || lower.contains("what's blocked")
        || lower.contains("whats blocked")
        || lower.starts_with("how is ")
        || lower.starts_with("how's ")
        || lower.contains("what happened")
        || (lower.ends_with('?')
            && [
                "task", "work", "running", "progress", "blocked", "done", "doing", "session",
            ]
            .iter()
            .any(|word| lower.contains(word)))
}
fn reply_like(text: &str) -> bool {
    matches!(
        text.trim().to_ascii_lowercase().as_str(),
        "yes" | "no" | "y" | "n" | "continue" | "approve" | "deny" | "do it" | "sounds good"
    )
}

fn cancel_request(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    lower == "stop"
        || lower == "cancel"
        || lower.starts_with("stop ")
        || lower.starts_with("cancel ")
}
fn route_hint(text: &str) -> Option<Provider> {
    let lower = text.to_ascii_lowercase();
    Provider::ALL.into_iter().find(|provider| {
        lower.contains(&format!("use {provider}")) || lower.contains(&format!("with {provider}"))
    })
}
async fn task_should_continue(
    store: &Store,
    task: &ManagedTask,
    outcome: &Outcome,
) -> Result<bool> {
    if task.cancel_requested
        || task.attempts.saturating_add(1) >= task.max_attempts
        || outcome.state != State::Idle
        || outcome.facts.terminal != Terminal::Completed
        || !outcome.facts.joined
        || outcome.facts.effects == EffectState::Uncertain
        || outcome.facts.pending_attention
        || outcome.facts.failure.is_some()
        || task.last_output.as_ref() == Some(&outcome.text)
    {
        return Ok(false);
    }
    let config = Config::load(store.root())?.0;
    if !config.extensions.judge.enabled {
        return Ok(false);
    }
    let Some(backend) = judge::resolve(store.root(), &config.extensions.judge)? else {
        return Ok(false);
    };
    let mut questions = judge::JudgeQuestions::new();
    questions.insert(
        "continue_task".into(),
        judge::JudgeQuestion::Noul {
            instructions: "Should the same coding task continue in its existing session? Answer true only when the worker plainly reports unfinished authorized work that can proceed without user input, approval, new permissions, or repeating an uncertain effect.".into(),
            criteria: Some(judge::NoulCriteria {
                r#true: Some("The original task remains unfinished and the next step is within its existing scope.".into()),
                r#false: Some("The task is complete, blocked, ambiguous, needs the user, or would expand scope.".into()),
            }),
        },
    );
    let answers = backend
        .ask(
            &json!({
                "task": xcb_core::display_text(&task.goal, 8192),
                "worker_response": xcb_core::display_text(&outcome.text, 8192),
                "attempt": task.attempts + 1,
                "maximum_attempts": task.max_attempts,
            }),
            &questions,
        )
        .await?;
    Ok(answers
        .answers
        .get("continue_task")
        .and_then(|answer| answer.noul())
        .is_some_and(|probability| probability >= 0.75))
}

fn workspace_busy(store: &Store, workspace: &str) -> Result<bool> {
    for run in store.unsettled_runs()? {
        if let Some(session) = run.session
            && store
                .session(&session)?
                .is_some_and(|session| session.workspace == workspace)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn worker_prompt(task: &ManagedTask, preferences: &[Preference]) -> String {
    let mut prompt = task.next_prompt.clone();
    prompt.push_str("\n\nXCB managed-task contract:\n- Work only on this task in the supplied workspace.\n- Run applicable checks before declaring completion.\n- If a material product choice, approval, credential, or missing input blocks you, ask one specific question and stop.\n- Do not commit, push, merge, deploy, or expand scope unless the task explicitly authorizes it.\n- Preserve uncertain effects and report them; never repeat an uncertain write.");
    if !preferences.is_empty() {
        prompt.push_str("\n\nUser preferences:\n");
        for preference in preferences.iter().take(16) {
            prompt.push_str("- ");
            prompt.push_str(&preference.text);
            prompt.push('\n');
        }
    }
    prompt
}

struct Completion {
    id: Id,
    result: Result<Outcome>,
}

pub async fn daemon(root: PathBuf) -> Result<i32> {
    let managed = Arc::new(ManagedStore::open(&root)?);
    let lock_path = managed.root().join("supervisor.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)?;
    private::check_file(&lock, 4096)?;
    match lock.try_lock() {
        Ok(()) => (),
        Err(std::fs::TryLockError::WouldBlock) => return Ok(0),
        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
    }
    let store = Arc::new(Store::open(&root)?);
    managed.reconcile_startup(&store).await?;
    let mut active: BTreeMap<Id, watch::Sender<bool>> = BTreeMap::new();
    let mut active_accounts: BTreeMap<Id, Id> = BTreeMap::new();
    let mut active_workspaces: BTreeMap<Id, String> = BTreeMap::new();
    let mut launch_attempts: BTreeMap<Id, Instant> = BTreeMap::new();
    let mut joins: JoinSet<Completion> = JoinSet::new();
    let mut idle_since = Instant::now();
    let mut interval = tokio::time::interval(Duration::from_millis(250));
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            _ = interval.tick() => {
                while let Some(joined) = joins.try_join_next() {
                    let completion = joined.map_err(|_| Error::Unavailable("managed worker task failed"))?;
                    active.remove(&completion.id);
                    active_accounts.remove(&completion.id);
                    active_workspaces.remove(&completion.id);
                    let finished = managed.finish(&store, &completion.id, completion.result).await?;
                    if finished.state == TaskState::Queued
                        && finished.detail.starts_with("dispatch did not cross")
                    {
                        launch_attempts.insert(completion.id, Instant::now());
                    } else {
                        launch_attempts.remove(&completion.id);
                    }
                }
                let tasks = managed.active_tasks(128)?;
                for task in &tasks {
                    if !task.cancel_requested { continue; }
                    if let Some(cancel) = active.get(&task.id) {
                        let _ = cancel.send(true);
                    } else if matches!(task.state, TaskState::Queued | TaskState::NeedsInput) {
                        match managed.settle_unstarted_cancel(task).await {
                            Ok(_) | Err(Error::Conflict(_)) => (),
                            Err(error) => return Err(error),
                        }
                    }
                }
                for task in tasks.into_iter().filter(|task| task.state == TaskState::Queued && !task.cancel_requested) {
                    if active.len() >= MAX_ACTIVE { break; }
                    if active.contains_key(&task.id)
                        || active_workspaces.values().any(|workspace| workspace == &task.workspace)
                        || launch_attempts
                            .get(&task.id)
                            .is_some_and(|attempt| attempt.elapsed() < Duration::from_secs(5))
                        || workspace_busy(&store, &task.workspace)?
                    {
                        continue;
                    }
                    launch_attempts.insert(task.id.clone(), Instant::now());
                    if !Path::new(&task.workspace).is_dir() {
                        let mut failed = task.clone();
                        failed.state = TaskState::Failed;
                        failed.detail = "workspace is unavailable".into();
                        failed.revision += 1;
                        failed.updated_at_ms = now_ms();
                        let message = ManagedStore::assistant(
                            format!("**{}** could not start because its workspace is unavailable.", task.title),
                            Some(&task.id),
                            failed.revision,
                        );
                        match managed.transition(&task, failed, Some(message)).await {
                            Ok(_) | Err(Error::Conflict(_)) => continue,
                            Err(error) => return Err(error),
                        }
                    }
                    let config = Config::load(store.root())?.0;
                    let created_session = task.session.is_none();
                    let session = if let Some(id) = &task.session {
                        store.session(id)?.ok_or(Error::Unavailable("managed worker session missing"))?
                    } else {
                        let hinted = task
                            .route
                            .as_deref()
                            .and_then(|route| route.parse::<Provider>().ok())
                            .map(|provider| kernel::choose_model(&store, provider, None, &config))
                            .transpose()?;
                        let judged = if hinted.is_none() && config.extensions.judge.enabled {
                            kernel::auto_route(&store, &config, &task.goal, None).await.ok()
                        } else {
                            None
                        };
                        let account = judged.as_ref().map(|(account, _)| account);
                        let model_key = hinted
                            .as_ref()
                            .map(|model| model.key())
                            .or_else(|| judged.as_ref().map(|(_, model)| model.key()));
                        match kernel::new_session(
                            &store,
                            Path::new(&task.workspace),
                            &config,
                            account,
                            model_key.as_deref(),
                        ) {
                            Ok(session) => session,
                            Err(Error::Conflict(_) | Error::Unavailable(_)) => continue,
                            Err(error) => {
                                let mut failed = task.clone();
                                failed.state = TaskState::Failed;
                                failed.detail = "worker route preparation failed".into();
                                failed.last_output = Some(error.to_string());
                                failed.revision += 1;
                                failed.updated_at_ms = now_ms();
                                let message = ManagedStore::assistant(
                                    format!("**{}** could not prepare a worker route: {error}", task.title),
                                    Some(&task.id),
                                    failed.revision,
                                );
                                match managed.transition(&task, failed, Some(message)).await {
                                    Ok(_) | Err(Error::Conflict(_)) => continue,
                                    Err(error) => return Err(error),
                                }
                            }
                        }
                    };
                    if active_accounts.values().any(|account| account == &session.account) {
                        if created_session {
                            store.remove_session(&session.id)?;
                        }
                        continue;
                    }
                    let route = format!("{} · {}", session.model.key(), store.account(&session.account)?.name());
                    let message_count = store.message_count(&session.id)?;
                    let prepared = match managed
                        .prepare(&task, session.id.clone(), route, message_count)
                        .await
                    {
                        Ok(task) => task,
                        Err(Error::Conflict(_)) => {
                            if created_session {
                                store.remove_session(&session.id)?;
                            }
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    let prompt = worker_prompt(&prepared, &managed.preferences(Path::new(&prepared.workspace))?);
                    let images = prepared.attachments.clone();
                    let id = prepared.id.clone();
                    let (cancel, cancelled) = watch::channel(false);
                    active.insert(id.clone(), cancel);
                    active_accounts.insert(id.clone(), session.account.clone());
                    active_workspaces.insert(id.clone(), prepared.workspace.clone());
                    let store = store.clone();
                    joins.spawn(async move {
                        let observer: Observer = Arc::new(|event| { if let Progress::Notice(_) | Progress::Tool(_) = event {} });
                        let result = kernel::execute(store, session.id, prompt, images, false, cancelled, observer).await;
                        Completion { id, result }
                    });
                }
                let nonterminal = !managed.active_tasks(1)?.is_empty();
                if active.is_empty() && !nonterminal {
                    if idle_since.elapsed() >= IDLE_EXIT { break; }
                } else { idle_since = Instant::now(); }
            }
            _ = interrupt.recv() => {
                for cancel in active.values() { let _ = cancel.send(true); }
                while let Some(joined) = joins.join_next().await {
                    let completion = joined.map_err(|_| Error::Unavailable("managed worker task failed"))?;
                    managed.finish(&store, &completion.id, completion.result).await?;
                }
                break;
            }
        }
    }
    Ok(0)
}

pub fn ensure_daemon(root: &Path, executable: &Path) -> Result<()> {
    if !executable.is_absolute() || !root.is_absolute() {
        return Err(Error::PrivateState);
    }
    let directory = private::directory(&root.join("managed"))?;
    let lock_path = directory.join("supervisor.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(lock_path)?;
    private::check_file(&lock, 4096)?;
    match lock.try_lock() {
        Ok(()) => drop(lock),
        Err(std::fs::TryLockError::WouldBlock) => return Ok(()),
        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
    }
    let mut command = Command::new(executable);
    command
        .arg("--state")
        .arg(root)
        .arg("managed-daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    command.spawn().map_err(Error::LaunchNotStarted)?;
    Ok(())
}

fn managed_view(
    store: &Store,
    managed: &ManagedStore,
    conversation: &Id,
    workspace: &Path,
) -> Result<View> {
    let config = Config::load(store.root())?.0;
    let mut view = summary::snapshot(store, None, &config, now_ms())?;
    view.conversation = Some(conversation.clone());
    view.conversations = managed
        .conversations(64)?
        .into_iter()
        .map(|conversation| ConversationRow {
            id: conversation.id,
            title: conversation.title,
            workspace: conversation.workspace,
            updated_at_ms: conversation.updated_at_ms,
        })
        .collect();
    view.messages = managed.messages(conversation, 256)?;
    let mut tasks = managed.active_tasks(128)?;
    let active_ids: BTreeSet<_> = tasks.iter().map(|task| task.id.clone()).collect();
    tasks.extend(
        managed
            .tasks(64)?
            .into_iter()
            .filter(|task| !active_ids.contains(&task.id))
            .take(16),
    );
    view.tasks = tasks
        .iter()
        .map(|task| TaskRow {
            id: task.id.clone(),
            title: task.title.clone(),
            state: task.state.ui(),
            detail: task.detail.clone(),
            route: task.route.clone(),
            workspace: task.workspace.clone(),
            updated_at_ms: task.updated_at_ms,
        })
        .collect();
    view.state = if tasks.iter().any(|task| task.state == TaskState::NeedsInput) {
        State::NeedsAnswer
    } else if tasks
        .iter()
        .any(|task| matches!(task.state, TaskState::Queued | TaskState::Running))
    {
        State::Working
    } else if tasks.iter().any(|task| task.state == TaskState::Uncertain) {
        State::Uncertain
    } else {
        State::Idle
    };
    view.pane = xcb_core::panes::Pane::focus();
    view.extensions.insert(
        0,
        (
            "algal supervisor".into(),
            format!(
                "on · {} tasks · {}",
                tasks.len(),
                workspace
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("workspace")
            ),
        ),
    );
    Ok(view)
}

pub async fn serve_ui(
    store: Arc<Store>,
    mut conversation: Id,
    input: Receiver<Intent>,
    output: SyncSender<Update>,
    executable: PathBuf,
) -> Result<()> {
    let managed = Arc::new(ManagedStore::open(store.root())?);
    let selected = managed
        .conversation(&conversation)?
        .ok_or(Error::Unavailable("managed conversation not found"))?;
    let mut workspace = PathBuf::from(selected.workspace);
    ensure_daemon(store.root(), &executable)?;
    let mut ticker = tokio::time::interval(Duration::from_millis(250));
    let mut quit = false;
    while !quit {
        ticker.tick().await;
        for _ in 0..32 {
            let intent = match input.try_recv() {
                Ok(intent) => intent,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => Intent::Quit,
            };
            match intent {
                Intent::Submit {
                    id,
                    text,
                    attachments,
                } => {
                    match managed
                        .submit(
                            &conversation,
                            id,
                            text.clone(),
                            attachments.clone(),
                            &workspace,
                        )
                        .await
                    {
                        Ok(()) => {
                            if let Err(error) = ensure_daemon(store.root(), &executable) {
                                output.try_send(Update::Notice(format!("Task was saved, but the background supervisor could not start: {error}"))).ok();
                            }
                        }
                        Err(error) => {
                            output.try_send(Update::Draft { text, attachments }).ok();
                            output
                                .try_send(Update::Notice(format!(
                                    "Message was not accepted: {error}"
                                )))
                                .ok();
                        }
                    }
                }
                Intent::Cancel => {
                    let id = new_id("m");
                    if let Err(error) = managed
                        .submit(&conversation, id, "cancel".into(), vec![], &workspace)
                        .await
                    {
                        output
                            .try_send(Update::Notice(format!(
                                "Cancellation was not accepted: {error}"
                            )))
                            .ok();
                    }
                }
                Intent::Quit => {
                    quit = true;
                    break;
                }
                Intent::Refresh => (),
                Intent::Conversation(id) => match managed.conversation(&id)? {
                    Some(selected) => {
                        conversation = selected.id;
                        workspace = PathBuf::from(selected.workspace);
                    }
                    None => {
                        output
                            .try_send(Update::Notice("Conversation not found.".into()))
                            .ok();
                    }
                },
                Intent::AttachPath(path) => {
                    match attachments::from_path(store.root(), Path::new(&path)) {
                        Ok(attachment) => {
                            output.try_send(Update::Attachment(attachment)).ok();
                        }
                        Err(error) => {
                            output
                                .try_send(Update::Notice(format!("Attachment rejected: {error}")))
                                .ok();
                        }
                    }
                }
                Intent::AttachRgba {
                    width,
                    height,
                    bytes,
                } => match attachments::from_rgba(store.root(), width, height, bytes) {
                    Ok(attachment) => {
                        output.try_send(Update::Attachment(attachment)).ok();
                    }
                    Err(error) => {
                        output
                            .try_send(Update::Notice(format!("Attachment rejected: {error}")))
                            .ok();
                    }
                },
                Intent::Account(_)
                | Intent::Model(_)
                | Intent::SetDefault
                | Intent::NewSession
                | Intent::Resume(_)
                | Intent::Pane(_)
                | Intent::SavePane { .. }
                | Intent::GeneratePane(_)
                | Intent::Extension { .. } => {
                    output.try_send(Update::Notice("The global dispatcher routes managed tasks automatically. Use `xcb resume` for direct provider-session controls.".into())).ok();
                }
            }
        }
        if !quit {
            output
                .try_send(Update::View(Box::new(managed_view(
                    &store,
                    &managed,
                    &conversation,
                    &workspace,
                )?)))
                .ok();
        }
    }
    output.try_send(Update::Stopped).ok();
    Ok(())
}

pub fn list(store: &ManagedStore) -> Result<Vec<ManagedTask>> {
    store.tasks(256)
}
pub fn inspect(store: &ManagedStore, id: &Id) -> Result<Option<ManagedTask>> {
    store.task(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn root() -> TempDir {
        tempfile::tempdir().unwrap()
    }
    fn workspace(root: &TempDir) -> PathBuf {
        root.path().canonicalize().unwrap()
    }
    fn message(name: &str) -> Id {
        Id::new(name).unwrap()
    }
    async fn conversation(store: &ManagedStore, workspace: &Path) -> Id {
        store.create_conversation(workspace).await.unwrap().id
    }

    #[tokio::test]
    async fn duplicate_client_message_creates_one_task_and_one_user_turn() {
        let root = root();
        let workspace = workspace(&root);
        let store = ManagedStore::open(&workspace).unwrap();
        let chat = conversation(&store, &workspace).await;
        store
            .submit(
                &chat,
                message("m_once"),
                "fix the bug".into(),
                vec![],
                &workspace,
            )
            .await
            .unwrap();
        store
            .submit(
                &chat,
                message("m_once"),
                "fix the bug".into(),
                vec![],
                &workspace,
            )
            .await
            .unwrap();
        assert_eq!(store.tasks(10).unwrap().len(), 1);
        assert_eq!(
            store
                .messages(&chat, 10)
                .unwrap()
                .iter()
                .filter(|m| m.role == Role::User)
                .count(),
            1
        );
        let task = store.tasks(10).unwrap().remove(0);
        assert!(task.last_receipt.starts_with("sha256:"));
        let payload: String = store
            .db()
            .unwrap()
            .query_row(
                "SELECT payload FROM receipts WHERE digest=?1",
                [&task.last_receipt],
                |row| row.get(0),
            )
            .unwrap();
        let receipt: Value = serde_json::from_str(&payload).unwrap();
        let manifest = Manifest::parse(&serde_json::from_str(POLICY).unwrap()).unwrap();
        let verified =
            runtime::verify(&receipt, manifest, &AlgalStore::default(), &Host::default())
                .await
                .unwrap();
        assert_eq!(verified["ok"], true);
    }

    #[tokio::test]
    async fn concurrent_conversations_isolate_transcripts_and_share_the_task_swarm() {
        let root = root();
        let workspace = workspace(&root);
        let store = ManagedStore::open(&workspace).unwrap();
        let first = conversation(&store, &workspace).await;
        let second = conversation(&store, &workspace).await;
        let (first_result, second_result) = tokio::join!(
            store.submit(
                &first,
                message("m_first_chat"),
                "first independent task".into(),
                vec![],
                &workspace,
            ),
            store.submit(
                &second,
                message("m_second_chat"),
                "second independent task".into(),
                vec![],
                &workspace,
            ),
        );
        first_result.unwrap();
        second_result.unwrap();
        assert_eq!(store.tasks(10).unwrap().len(), 2);
        let first_messages = store.messages(&first, 10).unwrap();
        let second_messages = store.messages(&second, 10).unwrap();
        assert!(
            first_messages
                .iter()
                .any(|message| message.text.contains("first independent"))
        );
        assert!(
            !first_messages
                .iter()
                .any(|message| message.text.contains("second independent"))
        );
        assert!(
            second_messages
                .iter()
                .any(|message| message.text.contains("second independent"))
        );
        store
            .submit(
                &second,
                message("m_swarm_status"),
                "what is running?".into(),
                vec![],
                &workspace,
            )
            .await
            .unwrap();
        assert!(
            store
                .messages(&second, 20)
                .unwrap()
                .last()
                .unwrap()
                .text
                .contains("2 active tasks")
        );
    }

    #[tokio::test]
    async fn one_pending_question_captures_the_next_conversational_reply() {
        let root = root();
        let workspace = workspace(&root);
        let store = ManagedStore::open(&workspace).unwrap();
        let chat = conversation(&store, &workspace).await;
        let task = store
            .create_task(
                &chat,
                message("m_task"),
                "choose an API".into(),
                vec![],
                &workspace,
            )
            .await
            .unwrap();
        let mut waiting = task.clone();
        waiting.state = TaskState::NeedsInput;
        waiting.detail = "Which API?".into();
        waiting.revision += 1;
        waiting.updated_at_ms = now_ms();
        let waiting = store.transition(&task, waiting, None).await.unwrap();
        store
            .submit(
                &chat,
                message("m_reply"),
                "Keep the public API".into(),
                vec![],
                &workspace,
            )
            .await
            .unwrap();
        let updated = store.task(&waiting.id).unwrap().unwrap();
        assert_eq!(updated.state, TaskState::Queued);
        assert_eq!(updated.next_prompt, "Keep the public API");
        assert_eq!(store.tasks(10).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn bare_answer_in_another_conversation_never_reaches_a_waiting_worker() {
        let root = root();
        let workspace = workspace(&root);
        let store = ManagedStore::open(&workspace).unwrap();
        let owner = conversation(&store, &workspace).await;
        let observer = conversation(&store, &workspace).await;
        let task = store
            .create_task(
                &owner,
                message("m_owner_task"),
                "choose the API".into(),
                vec![],
                &workspace,
            )
            .await
            .unwrap();
        let mut waiting = task.clone();
        waiting.state = TaskState::NeedsInput;
        waiting.detail = "Which API?".into();
        waiting.revision += 1;
        waiting.updated_at_ms = now_ms();
        store.transition(&task, waiting, None).await.unwrap();
        store
            .submit(
                &observer,
                message("m_other_yes"),
                "yes".into(),
                vec![],
                &workspace,
            )
            .await
            .unwrap();
        assert_eq!(store.tasks(10).unwrap().len(), 1);
        assert_eq!(
            store.task(&task.id).unwrap().unwrap().state,
            TaskState::NeedsInput
        );
        assert!(
            store
                .messages(&observer, 10)
                .unwrap()
                .last()
                .unwrap()
                .text
                .contains("task id or title")
        );
    }

    #[tokio::test]
    async fn ambiguous_answer_is_never_broadcast_to_multiple_waiting_tasks() {
        let root = root();
        let workspace = workspace(&root);
        let store = ManagedStore::open(&workspace).unwrap();
        let chat = conversation(&store, &workspace).await;
        for (message_id, text) in [("m_first", "first task"), ("m_second", "second task")] {
            let task = store
                .create_task(&chat, message(message_id), text.into(), vec![], &workspace)
                .await
                .unwrap();
            let mut waiting = task.clone();
            waiting.state = TaskState::NeedsInput;
            waiting.detail = "needs a decision".into();
            waiting.revision += 1;
            waiting.updated_at_ms = now_ms();
            store.transition(&task, waiting, None).await.unwrap();
        }
        store
            .submit(&chat, message("m_yes"), "yes".into(), vec![], &workspace)
            .await
            .unwrap();
        let tasks = store.tasks(10).unwrap();
        assert_eq!(tasks.len(), 2);
        assert!(tasks.iter().all(|task| task.state == TaskState::NeedsInput));
        assert!(
            store
                .messages(&chat, 20)
                .unwrap()
                .last()
                .unwrap()
                .text
                .contains("task id or title")
        );
    }

    #[tokio::test]
    async fn bare_cancellation_is_scoped_to_its_conversation() {
        let root = root();
        let workspace = workspace(&root);
        let store = ManagedStore::open(&workspace).unwrap();
        let first = conversation(&store, &workspace).await;
        let second = conversation(&store, &workspace).await;
        let first_task = store
            .create_task(
                &first,
                message("m_cancel_first"),
                "first scoped task".into(),
                vec![],
                &workspace,
            )
            .await
            .unwrap();
        let second_task = store
            .create_task(
                &second,
                message("m_cancel_second"),
                "second scoped task".into(),
                vec![],
                &workspace,
            )
            .await
            .unwrap();
        store
            .submit(
                &second,
                message("m_cancel_local"),
                "cancel".into(),
                vec![],
                &workspace,
            )
            .await
            .unwrap();
        assert!(
            !store
                .task(&first_task.id)
                .unwrap()
                .unwrap()
                .cancel_requested
        );
        assert!(
            store
                .task(&second_task.id)
                .unwrap()
                .unwrap()
                .cancel_requested
        );
    }

    #[tokio::test]
    async fn cancelling_queued_work_settles_without_claiming_a_worker_was_stopped() {
        let root = root();
        let workspace = workspace(&root);
        let store = ManagedStore::open(&workspace).unwrap();
        let chat = conversation(&store, &workspace).await;
        let task = store
            .create_task(
                &chat,
                message("m_cancel_task"),
                "wait for a route".into(),
                vec![],
                &workspace,
            )
            .await
            .unwrap();
        store
            .submit(
                &chat,
                message("m_cancel"),
                "cancel".into(),
                vec![],
                &workspace,
            )
            .await
            .unwrap();
        let requested = store.task(&task.id).unwrap().unwrap();
        assert!(requested.cancel_requested);
        let cancelled = store.settle_unstarted_cancel(&requested).await.unwrap();
        assert_eq!(cancelled.state, TaskState::Cancelled);
        assert_eq!(cancelled.detail, "cancelled before worker dispatch");
    }

    #[tokio::test]
    async fn startup_requeues_only_a_provably_unadmitted_dispatch_gap() {
        let root = root();
        let workspace = workspace(&root);
        let state = private::directory(&workspace.join("state")).unwrap();
        let managed = ManagedStore::open(&state).unwrap();
        let xcb = Store::open(&state).unwrap();
        let chat = conversation(&managed, &workspace).await;
        let task = managed
            .create_task(
                &chat,
                message("m_gap"),
                "do work".into(),
                vec![],
                &workspace,
            )
            .await
            .unwrap();
        let mut running = task.clone();
        running.state = TaskState::Running;
        running.detail = "dispatching".into();
        running.revision += 1;
        running.updated_at_ms = now_ms();
        let running = managed.transition(&task, running, None).await.unwrap();
        managed.reconcile_startup(&xcb).await.unwrap();
        assert_eq!(
            managed.task(&running.id).unwrap().unwrap().state,
            TaskState::Queued
        );
    }

    #[tokio::test]
    async fn startup_marks_an_admitted_unsettled_worker_uncertain_without_retry() {
        use xcb_core::models::{Mode, ModelChoice};
        let state_root = root();
        let workspace_root = root();
        let state =
            private::directory(&state_root.path().canonicalize().unwrap().join("state")).unwrap();
        let workspace = workspace_root.path().canonicalize().unwrap();
        let managed = ManagedStore::open(&state).unwrap();
        let xcb = Store::open(&state).unwrap();
        let account = xcb
            .add_account(Provider::Claude, "Max", now_ms(), None)
            .unwrap();
        let model = ModelChoice {
            provider: Provider::Claude,
            id: Id::new("sonnet").unwrap(),
            label: "Sonnet".into(),
            mode: Mode::Fixed,
            resolved: None,
            effort: Some(Id::new("high").unwrap()),
            observed_at_ms: now_ms(),
        };
        xcb.set_models(Provider::Claude, std::slice::from_ref(&model))
            .unwrap();
        let session = xcb
            .create_session(&account.id, model, &workspace, now_ms())
            .unwrap();
        let chat = conversation(&managed, &workspace).await;
        let task = managed
            .create_task(
                &chat,
                message("m_live"),
                "do work".into(),
                vec![],
                &workspace,
            )
            .await
            .unwrap();
        let mut running = task.clone();
        running.session = Some(session.id.clone());
        running.state = TaskState::Running;
        running.detail = "worker is running".into();
        running.revision += 1;
        running.updated_at_ms = now_ms();
        managed.transition(&task, running, None).await.unwrap();
        xcb.prepare_run(&session.id, session.revision, now_ms())
            .unwrap();
        assert!(workspace_busy(&xcb, workspace.to_str().unwrap()).unwrap());

        managed.reconcile_startup(&xcb).await.unwrap();
        let task = managed.task(&task.id).unwrap().unwrap();
        assert_eq!(task.state, TaskState::Uncertain);
        assert!(task.detail.contains("explicit recovery"));
        assert_eq!(xcb.unsettled_runs().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn retry_requires_positive_evidence_that_provider_dispatch_never_started() {
        use xcb_core::models::{Mode, ModelChoice};
        let state_root = root();
        let workspace_root = root();
        let state =
            private::directory(&state_root.path().canonicalize().unwrap().join("state")).unwrap();
        let workspace = workspace_root.path().canonicalize().unwrap();
        let managed = ManagedStore::open(&state).unwrap();
        let xcb = Store::open(&state).unwrap();
        let account = xcb
            .add_account(Provider::Claude, "Max", now_ms(), None)
            .unwrap();
        let model = ModelChoice {
            provider: Provider::Claude,
            id: Id::new("sonnet").unwrap(),
            label: "Sonnet".into(),
            mode: Mode::Fixed,
            resolved: None,
            effort: Some(Id::new("high").unwrap()),
            observed_at_ms: now_ms(),
        };
        let chat = conversation(&managed, &workspace).await;
        let first_session = xcb
            .create_session(&account.id, model.clone(), &workspace, now_ms())
            .unwrap();
        let first = managed
            .create_task(
                &chat,
                message("m_not_started"),
                "one".into(),
                vec![],
                &workspace,
            )
            .await
            .unwrap();
        let first = managed
            .prepare(&first, first_session.id.clone(), model.key(), 0)
            .await
            .unwrap();
        let first = managed
            .finish(&xcb, &first.id, Err(Error::Unavailable("route busy")))
            .await
            .unwrap();
        assert_eq!(first.state, TaskState::Queued);

        let second_session = xcb
            .create_session(&account.id, model, &workspace, now_ms())
            .unwrap();
        let second = managed
            .create_task(
                &chat,
                message("m_crossed"),
                "two".into(),
                vec![],
                &workspace,
            )
            .await
            .unwrap();
        let second = managed
            .prepare(
                &second,
                second_session.id.clone(),
                "claude/sonnet/high".into(),
                0,
            )
            .await
            .unwrap();
        xcb.append_message(
            &second_session.id,
            second_session.revision,
            &Message {
                id: message("m_provider_user"),
                role: Role::User,
                text: "two".into(),
                at_ms: now_ms(),
                attachments: vec![],
                provenance: None,
            },
        )
        .unwrap();
        let second = managed
            .finish(&xcb, &second.id, Err(Error::Unavailable("transport ended")))
            .await
            .unwrap();
        assert_eq!(second.state, TaskState::Uncertain);
        assert!(second.detail.contains("no automatic retry"));
    }

    #[test]
    fn learned_routes_require_repeated_success_and_reverse_after_failures() {
        let root = root();
        let workspace = workspace(&root);
        let store = ManagedStore::open(&workspace).unwrap();
        {
            let db = store.db().unwrap();
            db.execute(
                "INSERT INTO route_stats(scope,provider,completed,failed) VALUES(?1,'claude',2,0)",
                [workspace.to_str().unwrap()],
            )
            .unwrap();
        }
        assert_eq!(
            store.learned_route(&workspace).unwrap(),
            Some(Provider::Claude)
        );
        {
            let db = store.db().unwrap();
            db.execute(
                "UPDATE route_stats SET failed=2 WHERE scope=?1 AND provider='claude'",
                [workspace.to_str().unwrap()],
            )
            .unwrap();
        }
        assert_eq!(store.learned_route(&workspace).unwrap(), None);
    }

    #[tokio::test]
    async fn detaching_the_ui_does_not_cancel_or_delete_accepted_work() {
        let state_root = root();
        let workspace_root = root();
        let state =
            private::directory(&state_root.path().canonicalize().unwrap().join("state")).unwrap();
        let workspace = workspace_root.path().canonicalize().unwrap();
        let xcb = Arc::new(Store::open(&state).unwrap());
        let managed = ManagedStore::open(&state).unwrap();
        let chat = conversation(&managed, &workspace).await;
        let (commands, input) = std::sync::mpsc::sync_channel(8);
        let (updates, _display) = std::sync::mpsc::sync_channel(8);
        let task = tokio::spawn(serve_ui(
            xcb,
            chat.clone(),
            input,
            updates,
            PathBuf::from("/usr/bin/true"),
        ));
        commands
            .send(Intent::Submit {
                id: message("m_detach"),
                text: "keep working".into(),
                attachments: vec![],
            })
            .unwrap();
        tokio::time::sleep(Duration::from_millis(350)).await;
        if task.is_finished() {
            panic!("managed UI exited early: {:?}", task.await.unwrap());
        }
        commands.send(Intent::Quit).unwrap();
        task.await.unwrap().unwrap();
        let managed = ManagedStore::open(&state).unwrap();
        let tasks = managed.tasks(10).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].state, TaskState::Queued);
        assert!(!tasks[0].cancel_requested);
    }

    #[tokio::test]
    async fn preference_is_scoped_and_injected_without_becoming_a_task() {
        let root = root();
        let workspace = workspace(&root);
        let store = ManagedStore::open(&workspace).unwrap();
        let chat = conversation(&store, &workspace).await;
        store
            .submit(
                &chat,
                message("m_pref"),
                "remember: keep updates concise".into(),
                vec![],
                &workspace,
            )
            .await
            .unwrap();
        assert!(store.tasks(10).unwrap().is_empty());
        let preferences = store.preferences(&workspace).unwrap();
        assert_eq!(preferences[0].text, "keep updates concise");
        store
            .submit(
                &chat,
                message("m_memory"),
                "what do you remember about this workspace?".into(),
                vec![],
                &workspace,
            )
            .await
            .unwrap();
        assert!(
            store
                .messages(&chat, 20)
                .unwrap()
                .last()
                .unwrap()
                .text
                .contains("keep updates concise")
        );
        let task = ManagedTask {
            version: 1,
            id: message("t_x"),
            operation: message("op_x"),
            source_message: message("m_x"),
            conversation: chat,
            workspace: workspace.to_string_lossy().into(),
            title: "x".into(),
            goal: "x".into(),
            next_prompt: "x".into(),
            attachments: vec![],
            session: None,
            route: None,
            state: TaskState::Queued,
            detail: "x".into(),
            attempts: 0,
            max_attempts: 8,
            message_count_before: 0,
            cancel_requested: false,
            last_output: None,
            policy_digest: "sha256:x".into(),
            last_receipt: "sha256:x".into(),
            revision: 1,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        assert!(worker_prompt(&task, &preferences).contains("keep updates concise"));
    }
}
