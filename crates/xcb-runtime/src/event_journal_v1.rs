//! Authoritative local event journal for revisioned managed state and receipts.
//!
//! This is a small SQLite-backed append-only event seam. Stable event IDs are
//! derived from an entity, typed event kind, and managed revision. Event bodies
//! are canonical JSON projections, never provider transcripts or credentials.
//! Causal parents point at the previous local head by event digest, so an old
//! writer cannot silently fork a task's history. Reads and replay are bounded
//! and may be resumed with an opaque sequence cursor.

use crate::{Error, Result, digest, now_ms};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

pub const JOURNAL_SCHEMA: &str = "xcb.local-event-journal.v1";
pub const MAX_EVENT_BODY_BYTES: usize = 64 * 1024;
pub const MAX_PAGE_SIZE: usize = 256;
pub const MAX_REPLAY_EVENTS: usize = 4_096;
const MAX_ID_BYTES: usize = 160;
const MAX_CURSOR_BYTES: usize = 32;
const MAX_REPLAY_RECEIPTS: usize = 512;
const MAX_REPLAY_COMMANDS: usize = 512;
const JOURNAL_VERSION: u32 = 1;

/// Typed records projected from managed state and local receipts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    State,
    Receipt,
    CommandAccepted,
    CommandSettled,
    Fault,
}
impl EventType {
    pub const ALL: [Self; 5] = [
        Self::State,
        Self::Receipt,
        Self::CommandAccepted,
        Self::CommandSettled,
        Self::Fault,
    ];
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Receipt => "receipt",
            Self::CommandAccepted => "command_accepted",
            Self::CommandSettled => "command_settled",
            Self::Fault => "fault",
        }
    }
    fn parse(value: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == value)
            .ok_or(Error::Conflict("event type is unknown"))
    }
}

/// Causal edge to the event that was the local head when this event was
/// accepted. The digest covers the parent's complete semantic event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CausalParent {
    pub id: String,
    pub digest: String,
}

/// Typed event body. Values are bounded managed projections supplied by the
/// caller; the journal never turns raw provider output into authority.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EventBody {
    State {
        snapshot: Value,
    },
    Receipt {
        receipt_id: String,
        receipt: Value,
    },
    CommandAccepted {
        command_id: String,
        command: Value,
    },
    CommandSettled {
        command_id: String,
        receipt_id: String,
        result: Value,
    },
    Fault {
        code: String,
        detail: String,
    },
}

/// An event before local sequence and content digests are assigned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventDraft {
    pub id: String,
    pub event_type: EventType,
    pub entity_id: String,
    pub revision: u64,
    pub parent: Option<CausalParent>,
    pub idempotency_key: Option<String>,
    pub body: EventBody,
    /// Evidence time is not part of the semantic digest, which makes retrying
    /// after a crash idempotent even when the wall clock moved.
    pub occurred_at_ms: u64,
}

impl EventDraft {
    pub fn state(
        entity_id: impl Into<String>,
        revision: u64,
        snapshot: Value,
        parent: Option<CausalParent>,
    ) -> Result<Self> {
        Self::new(
            EventType::State,
            entity_id,
            revision,
            parent,
            EventBody::State { snapshot },
        )
    }
    pub fn receipt(
        entity_id: impl Into<String>,
        revision: u64,
        receipt_id: impl Into<String>,
        receipt: Value,
        parent: Option<CausalParent>,
    ) -> Result<Self> {
        Self::new(
            EventType::Receipt,
            entity_id,
            revision,
            parent,
            EventBody::Receipt {
                receipt_id: receipt_id.into(),
                receipt,
            },
        )
    }
    pub fn command_accepted(
        entity_id: impl Into<String>,
        revision: u64,
        command_id: impl Into<String>,
        command: Value,
        parent: Option<CausalParent>,
    ) -> Result<Self> {
        Self::new(
            EventType::CommandAccepted,
            entity_id,
            revision,
            parent,
            EventBody::CommandAccepted {
                command_id: command_id.into(),
                command,
            },
        )
    }
    pub fn command_settled(
        entity_id: impl Into<String>,
        revision: u64,
        command_id: impl Into<String>,
        receipt_id: impl Into<String>,
        result: Value,
        parent: Option<CausalParent>,
    ) -> Result<Self> {
        Self::new(
            EventType::CommandSettled,
            entity_id,
            revision,
            parent,
            EventBody::CommandSettled {
                command_id: command_id.into(),
                receipt_id: receipt_id.into(),
                result,
            },
        )
    }
    pub fn fault(
        entity_id: impl Into<String>,
        revision: u64,
        code: impl Into<String>,
        detail: impl Into<String>,
        parent: Option<CausalParent>,
    ) -> Result<Self> {
        Self::new(
            EventType::Fault,
            entity_id,
            revision,
            parent,
            EventBody::Fault {
                code: code.into(),
                detail: detail.into(),
            },
        )
    }
    fn new(
        event_type: EventType,
        entity_id: impl Into<String>,
        revision: u64,
        parent: Option<CausalParent>,
        body: EventBody,
    ) -> Result<Self> {
        let entity_id = entity_id.into();
        let id = stable_event_id(&entity_id, event_type, revision)?;
        let draft = Self {
            id,
            event_type,
            entity_id,
            revision,
            parent,
            idempotency_key: None,
            body,
            occurred_at_ms: now_ms(),
        };
        draft.validate()?;
        Ok(draft)
    }
    pub fn with_idempotency_key(mut self, key: impl Into<String>) -> Result<Self> {
        self.idempotency_key = Some(key.into());
        self.validate()?;
        Ok(self)
    }
    pub fn with_occurred_at_ms(mut self, occurred_at_ms: u64) -> Result<Self> {
        self.occurred_at_ms = occurred_at_ms;
        self.validate()?;
        Ok(self)
    }
    pub fn body_digest(&self) -> Result<String> {
        body_digest(&self.body)
    }
    pub fn stable_id(&self) -> &str {
        &self.id
    }
    fn validate(&self) -> Result<()> {
        validate_id(&self.id)?;
        validate_id(&self.entity_id)?;
        if self.revision == 0 || self.revision > i64::MAX as u64 {
            return Err(Error::Conflict("event revision is invalid"));
        }
        if self.id != stable_event_id(&self.entity_id, self.event_type, self.revision)? {
            return Err(Error::Conflict("event id is not stable for its revision"));
        }
        validate_parent(self.parent.as_ref())?;
        if let Some(key) = &self.idempotency_key {
            validate_id(key)?;
        }
        validate_body(self.event_type, &self.body)?;
        sql_u64(self.occurred_at_ms, "event timestamp is invalid")?;
        Ok(())
    }
}

/// A committed immutable event. Sequence is local append order; stable ID and
/// digest do not depend on it, so replay after a restart is deterministic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JournalEvent {
    pub sequence: u64,
    pub id: String,
    pub event_type: EventType,
    pub entity_id: String,
    pub revision: u64,
    pub parent: Option<CausalParent>,
    pub idempotency_key: Option<String>,
    pub body_digest: String,
    pub event_digest: String,
    pub body: EventBody,
    pub occurred_at_ms: u64,
}
impl JournalEvent {
    pub fn causal_parent(&self) -> CausalParent {
        CausalParent {
            id: self.id.clone(),
            digest: self.event_digest.clone(),
        }
    }
    pub fn receipt_digest(&self) -> Option<String> {
        match &self.body {
            EventBody::Receipt { receipt, .. }
            | EventBody::CommandSettled {
                result: receipt, ..
            } => body_value_digest(receipt).ok(),
            _ => None,
        }
    }
    fn validate_integrity(&self) -> Result<()> {
        validate_id(&self.id)?;
        validate_id(&self.entity_id)?;
        if self.revision == 0 || self.revision > i64::MAX as u64 {
            return Err(Error::Conflict("event revision is invalid"));
        }
        if self.id != stable_event_id(&self.entity_id, self.event_type, self.revision)? {
            return Err(Error::Conflict("event stable id changed"));
        }
        validate_parent(self.parent.as_ref())?;
        if let Some(key) = &self.idempotency_key {
            validate_id(key)?;
        }
        validate_body(self.event_type, &self.body)?;
        let body_digest = body_digest(&self.body)?;
        if self.body_digest != body_digest {
            return Err(Error::Conflict("event body digest changed"));
        }
        let draft = EventDraft {
            id: self.id.clone(),
            event_type: self.event_type,
            entity_id: self.entity_id.clone(),
            revision: self.revision,
            parent: self.parent.clone(),
            idempotency_key: self.idempotency_key.clone(),
            body: self.body.clone(),
            occurred_at_ms: self.occurred_at_ms,
        };
        if self.event_digest != draft_event_digest(&draft, &body_digest)? {
            return Err(Error::Conflict("event digest changed"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppendOutcome {
    Appended(JournalEvent),
    Duplicate(JournalEvent),
}
impl AppendOutcome {
    pub fn event(&self) -> &JournalEvent {
        match self {
            Self::Appended(event) | Self::Duplicate(event) => event,
        }
    }
    pub fn is_duplicate(&self) -> bool {
        matches!(self, Self::Duplicate(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventCursor {
    pub after_sequence: u64,
}
impl EventCursor {
    pub fn encode(self) -> String {
        format!("s:{}", self.after_sequence)
    }
    pub fn decode(value: &str) -> Result<Self> {
        if value.len() > MAX_CURSOR_BYTES {
            return Err(Error::Conflict("event cursor is invalid"));
        }
        let after_sequence = value
            .strip_prefix("s:")
            .ok_or(Error::Conflict("event cursor is invalid"))?
            .parse::<u64>()
            .map_err(|_| Error::Conflict("event cursor is invalid"))?;
        if after_sequence == 0 {
            return Err(Error::Conflict("event cursor is invalid"));
        }
        Ok(Self { after_sequence })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventPage {
    pub events: Vec<JournalEvent>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReceiptProjection {
    pub event_id: String,
    pub revision: u64,
    pub receipt_id: String,
    pub receipt: Value,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplaySlice {
    pub entity_id: String,
    pub events: Vec<JournalEvent>,
    pub state: Option<Value>,
    pub state_revision: Option<u64>,
    pub receipts: Vec<ReceiptProjection>,
    pub pending_commands: Vec<String>,
    pub faults: Vec<String>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Replay {
    pub entity_id: String,
    pub events: Vec<JournalEvent>,
    pub state: Option<Value>,
    pub state_revision: Option<u64>,
    pub receipts: Vec<ReceiptProjection>,
    pub pending_commands: Vec<String>,
    pub faults: Vec<String>,
    pub head: Option<CausalParent>,
}

/// One private local SQLite file is the source of truth. The mutex makes the
/// read/append/restart boundaries explicit for callers sharing a process.
pub struct EventJournal {
    path: PathBuf,
    connection: Mutex<Connection>,
}
impl std::fmt::Debug for EventJournal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventJournal")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}
impl EventJournal {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if path.as_os_str().is_empty() {
            return Err(Error::PrivateState);
        }
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut connection = Connection::open(&path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF;",
        )?;
        migrate(&mut connection)?;
        Ok(Self {
            path,
            connection: Mutex::new(connection),
        })
    }
    pub fn open_in(directory: impl AsRef<Path>) -> Result<Self> {
        Self::open(directory.as_ref().join("events.sqlite"))
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    fn db(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| Error::Conflict("event journal lock is poisoned"))
    }

    /// Append atomically. A duplicate stable ID or idempotency key returns the
    /// original event. Any semantic body/causal change conflicts and cannot
    /// overwrite the local authoritative record.
    pub fn append(&self, draft: EventDraft) -> Result<AppendOutcome> {
        draft.validate()?;
        let body_digest = draft.body_digest()?;
        let event_digest = draft_event_digest(&draft, &body_digest)?;
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = find_event_by_id(&tx, &draft.id)? {
            if existing.event_digest == event_digest {
                tx.commit()?;
                return Ok(AppendOutcome::Duplicate(existing));
            }
            return Err(Error::Conflict("event body or causal metadata changed"));
        }
        if let Some(key) = &draft.idempotency_key {
            if let Some(existing) = find_event_by_key(&tx, key)? {
                if existing.event_digest == event_digest {
                    tx.commit()?;
                    return Ok(AppendOutcome::Duplicate(existing));
                }
                return Err(Error::Conflict(
                    "idempotency key was reused with another body",
                ));
            }
        }
        let head = find_head(&tx, &draft.entity_id)?;
        validate_causality(&tx, head.as_ref(), draft.parent.as_ref(), draft.revision)?;
        let event = JournalEvent {
            sequence: next_sequence(&tx)?,
            id: draft.id.clone(),
            event_type: draft.event_type,
            entity_id: draft.entity_id.clone(),
            revision: draft.revision,
            parent: draft.parent.clone(),
            idempotency_key: draft.idempotency_key.clone(),
            body_digest,
            event_digest,
            body: draft.body.clone(),
            occurred_at_ms: draft.occurred_at_ms,
        };
        let body = canonical_body(&event.body)?;
        tx.execute(
            "INSERT INTO journal_events(sequence,id,event_type,entity_id,revision,parent_id,parent_digest,idempotency_key,body_digest,event_digest,body,occurred_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                sql_u64(event.sequence, "event sequence")?,
                &event.id,
                event.event_type.as_str(),
                &event.entity_id,
                sql_u64(event.revision, "event revision")?,
                event.parent.as_ref().map(|parent| parent.id.as_str()),
                event.parent.as_ref().map(|parent| parent.digest.as_str()),
                event.idempotency_key.as_deref(),
                &event.body_digest,
                &event.event_digest,
                body,
                sql_u64(event.occurred_at_ms, "event timestamp is invalid")?,
            ],
        )?;
        tx.execute(
            "INSERT INTO journal_heads(entity_id,sequence,event_id,event_digest,revision) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(entity_id) DO UPDATE SET sequence=excluded.sequence,event_id=excluded.event_id,event_digest=excluded.event_digest,revision=excluded.revision",
            params![
                &event.entity_id,
                sql_u64(event.sequence, "event sequence")?,
                &event.id,
                &event.event_digest,
                sql_u64(event.revision, "event revision")?,
            ],
        )?;
        tx.commit()?;
        Ok(AppendOutcome::Appended(event))
    }
    pub fn append_state(
        &self,
        entity_id: impl Into<String>,
        revision: u64,
        snapshot: Value,
        parent: Option<CausalParent>,
    ) -> Result<AppendOutcome> {
        self.append(EventDraft::state(entity_id, revision, snapshot, parent)?)
    }
    pub fn append_receipt(
        &self,
        entity_id: impl Into<String>,
        revision: u64,
        receipt_id: impl Into<String>,
        receipt: Value,
        parent: Option<CausalParent>,
    ) -> Result<AppendOutcome> {
        self.append(EventDraft::receipt(
            entity_id, revision, receipt_id, receipt, parent,
        )?)
    }

    /// Read at most `limit` events. A limit+1 probe only determines `has_more`
    /// and is never returned, keeping every page bounded.
    pub fn read_page(
        &self,
        entity_id: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<EventPage> {
        validate_limit(limit)?;
        if let Some(entity_id) = entity_id {
            validate_id(entity_id)?;
        }
        let after = cursor
            .map(EventCursor::decode)
            .transpose()?
            .map_or(0, |cursor| cursor.after_sequence);
        let db = self.db()?;
        let fetch = sql_u64((limit + 1) as u64, "event page")?;
        let rows = if let Some(entity_id) = entity_id {
            let mut statement = db.prepare("SELECT sequence,id,event_type,entity_id,revision,parent_id,parent_digest,idempotency_key,body_digest,event_digest,body,occurred_at_ms FROM journal_events WHERE entity_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3")?;
            statement
                .query_map(
                    params![entity_id, sql_u64(after, "event cursor")?, fetch],
                    load_row,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            let mut statement = db.prepare("SELECT sequence,id,event_type,entity_id,revision,parent_id,parent_digest,idempotency_key,body_digest,event_digest,body,occurred_at_ms FROM journal_events WHERE sequence>?1 ORDER BY sequence LIMIT ?2")?;
            statement
                .query_map(params![sql_u64(after, "event cursor")?, fetch], load_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut events = Vec::with_capacity(limit);
        for row in rows {
            events.push(decode_row(row)?);
        }
        let has_more = events.len() > limit;
        if has_more {
            events.truncate(limit);
        }
        let next_cursor = has_more
            .then(|| {
                events.last().map(|event| {
                    EventCursor {
                        after_sequence: event.sequence,
                    }
                    .encode()
                })
            })
            .flatten();
        Ok(EventPage {
            events,
            next_cursor,
            has_more,
        })
    }
    pub fn read_events(
        &self,
        entity_id: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<EventPage> {
        self.read_page(entity_id, cursor, limit)
    }
    pub fn event(&self, id: &str) -> Result<Option<JournalEvent>> {
        validate_id(id)?;
        let db = self.db()?;
        find_event_by_id(&db, id)
    }
    pub fn replay_slice(
        &self,
        entity_id: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<ReplaySlice> {
        validate_id(entity_id)?;
        let page = self.read_page(Some(entity_id), cursor, limit)?;
        let projection = fold(entity_id, &page.events)?;
        Ok(ReplaySlice {
            entity_id: entity_id.to_owned(),
            events: page.events,
            state: projection.state,
            state_revision: projection.state_revision,
            receipts: projection.receipts,
            pending_commands: projection.pending_commands,
            faults: projection.faults,
            next_cursor: page.next_cursor,
            has_more: page.has_more,
        })
    }
    pub fn replay(&self, entity_id: &str, max_events: usize) -> Result<Replay> {
        validate_id(entity_id)?;
        if !(1..=MAX_REPLAY_EVENTS).contains(&max_events) {
            return Err(xcb_core::Error::Limit("event replay").into());
        }
        let page = self.read_page(Some(entity_id), None, max_events)?;
        if page.has_more {
            return Err(xcb_core::Error::Limit("event replay").into());
        }
        let head = page.events.last().map(JournalEvent::causal_parent);
        let projection = fold(entity_id, &page.events)?;
        Ok(Replay {
            entity_id: entity_id.to_owned(),
            events: page.events,
            state: projection.state,
            state_revision: projection.state_revision,
            receipts: projection.receipts,
            pending_commands: projection.pending_commands,
            faults: projection.faults,
            head,
        })
    }
    pub fn verify(&self, max_events: usize) -> Result<usize> {
        if !(1..=MAX_REPLAY_EVENTS).contains(&max_events) {
            return Err(xcb_core::Error::Limit("event verification").into());
        }
        let page = self.read_page(None, None, max_events)?;
        if page.has_more {
            return Err(xcb_core::Error::Limit("event verification").into());
        }
        let mut last = std::collections::BTreeMap::new();
        for event in &page.events {
            event.validate_integrity()?;
            if let Some(parent) = &event.parent {
                if let Some(previous) = page
                    .events
                    .iter()
                    .find(|candidate| candidate.id == parent.id)
                {
                    if previous.sequence >= event.sequence || previous.event_digest != parent.digest
                    {
                        return Err(Error::Conflict("event causal order changed"));
                    }
                }
            }
            last.insert(event.entity_id.clone(), event.clone());
        }
        let db = self.db()?;
        for (entity, event) in last {
            let indexed =
                find_head(&db, &entity)?.ok_or(Error::Conflict("event head is missing"))?;
            if indexed.id != event.id || indexed.event_digest != event.event_digest {
                return Err(Error::Conflict("event head index changed"));
            }
        }
        Ok(page.events.len())
    }
}

#[derive(Default)]
struct Projection {
    state: Option<Value>,
    state_revision: Option<u64>,
    receipts: Vec<ReceiptProjection>,
    pending_commands: Vec<String>,
    faults: Vec<String>,
}
fn fold(entity_id: &str, events: &[JournalEvent]) -> Result<Projection> {
    let mut projection = Projection::default();
    let mut seen = std::collections::BTreeSet::new();
    for event in events {
        if event.entity_id != entity_id || !seen.insert(event.id.clone()) {
            return Err(Error::Conflict("event replay identity changed"));
        }
        event.validate_integrity()?;
        match &event.body {
            EventBody::State { snapshot } => {
                projection.state = Some(snapshot.clone());
                projection.state_revision = Some(event.revision);
            }
            EventBody::Receipt {
                receipt_id,
                receipt,
            } => {
                if projection.receipts.len() >= MAX_REPLAY_RECEIPTS {
                    return Err(xcb_core::Error::Limit("event replay receipts").into());
                }
                projection.receipts.push(ReceiptProjection {
                    event_id: event.id.clone(),
                    revision: event.revision,
                    receipt_id: receipt_id.clone(),
                    receipt: receipt.clone(),
                    digest: body_value_digest(receipt)?,
                });
            }
            EventBody::CommandAccepted { command_id, .. } => {
                if projection.pending_commands.len() >= MAX_REPLAY_COMMANDS {
                    return Err(xcb_core::Error::Limit("event replay commands").into());
                }
                if !projection.pending_commands.contains(command_id) {
                    projection.pending_commands.push(command_id.clone());
                }
            }
            EventBody::CommandSettled { command_id, .. } => {
                projection.pending_commands.retain(|id| id != command_id)
            }
            EventBody::Fault { code, .. } => projection.faults.push(code.clone()),
        }
    }
    Ok(projection)
}

fn migrate(db: &mut Connection) -> Result<()> {
    let version: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > JOURNAL_VERSION {
        return Err(Error::Unavailable(
            "event journal schema is newer than this build",
        ));
    }
    if version != JOURNAL_VERSION {
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS journal_events(sequence INTEGER PRIMARY KEY,id TEXT NOT NULL UNIQUE,event_type TEXT NOT NULL,entity_id TEXT NOT NULL,revision INTEGER NOT NULL,parent_id TEXT,parent_digest TEXT,idempotency_key TEXT UNIQUE,body_digest TEXT NOT NULL,event_digest TEXT NOT NULL UNIQUE,body TEXT NOT NULL,occurred_at_ms INTEGER NOT NULL,CHECK((parent_id IS NULL)=(parent_digest IS NULL))); CREATE INDEX IF NOT EXISTS journal_events_entity_sequence ON journal_events(entity_id,sequence); CREATE TABLE IF NOT EXISTS journal_heads(entity_id TEXT PRIMARY KEY,sequence INTEGER NOT NULL,event_id TEXT NOT NULL UNIQUE,event_digest TEXT NOT NULL,revision INTEGER NOT NULL); PRAGMA user_version=1;")?;
        tx.commit()?;
    }
    Ok(())
}

/// Deterministic stable ID. It is public so other local projections can
/// precompute the identity before attempting an append.
pub fn stable_event_id(entity_id: &str, event_type: EventType, revision: u64) -> Result<String> {
    validate_id(entity_id)?;
    if revision == 0 || revision > i64::MAX as u64 {
        return Err(Error::Conflict("event revision is invalid"));
    }
    Ok(format!(
        "evt_{}",
        digest(format!(
            "xcb-local-event-id-v1\0{}\0{}\0{}",
            entity_id,
            event_type.as_str(),
            revision
        ))
    ))
}
fn validate_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-[]".contains(&byte))
    {
        return Err(Error::Conflict("event identifier is invalid"));
    }
    Ok(())
}
fn validate_digest(value: &str) -> Result<()> {
    if value.len() != 71 || !value.starts_with("sha256:") || !xcb_core::hex64(&value[7..]) {
        return Err(Error::Conflict("event digest is invalid"));
    }
    Ok(())
}
fn validate_parent(parent: Option<&CausalParent>) -> Result<()> {
    if let Some(parent) = parent {
        validate_id(&parent.id)?;
        validate_digest(&parent.digest)?;
    }
    Ok(())
}
fn validate_value(value: &Value) -> Result<()> {
    let encoded = xcb_core::protocol::canonical_json(value)
        .map_err(|_| Error::Conflict("event value is not canonical JSON"))?;
    if encoded.len() > MAX_EVENT_BODY_BYTES {
        return Err(xcb_core::Error::Limit("event value").into());
    }
    Ok(())
}
fn validate_body(event_type: EventType, body: &EventBody) -> Result<()> {
    let compatible = matches!(
        (event_type, body),
        (EventType::State, EventBody::State { .. })
            | (EventType::Receipt, EventBody::Receipt { .. })
            | (
                EventType::CommandAccepted,
                EventBody::CommandAccepted { .. }
            )
            | (EventType::CommandSettled, EventBody::CommandSettled { .. })
            | (EventType::Fault, EventBody::Fault { .. })
    );
    if !compatible {
        return Err(Error::Conflict("event type and body disagree"));
    }
    match body {
        EventBody::State { snapshot } => validate_value(snapshot)?,
        EventBody::Receipt {
            receipt_id,
            receipt,
        } => {
            validate_id(receipt_id)?;
            validate_value(receipt)?;
        }
        EventBody::CommandAccepted {
            command_id,
            command,
        } => {
            validate_id(command_id)?;
            validate_value(command)?;
        }
        EventBody::CommandSettled {
            command_id,
            receipt_id,
            result,
        } => {
            validate_id(command_id)?;
            validate_id(receipt_id)?;
            validate_value(result)?;
        }
        EventBody::Fault { code, detail } => {
            validate_id(code)?;
            if detail.len() > 2_048 || detail.chars().any(char::is_control) {
                return Err(Error::Conflict("event fault detail is invalid"));
            }
        }
    }
    if canonical_body(body)?.len() > MAX_EVENT_BODY_BYTES {
        return Err(xcb_core::Error::Limit("event body").into());
    }
    Ok(())
}
fn canonical_body(body: &EventBody) -> Result<String> {
    let value = serde_json::to_value(body)?;
    xcb_core::protocol::canonical_json(&value)
        .map_err(|_| Error::Conflict("event body is not canonical JSON"))
}
fn body_value_digest(value: &Value) -> Result<String> {
    validate_value(value)?;
    Ok(format!(
        "sha256:{}",
        digest(
            xcb_core::protocol::canonical_json(value)
                .map_err(|_| Error::Conflict("event value is not canonical JSON"))?
        )
    ))
}
fn body_digest(body: &EventBody) -> Result<String> {
    Ok(format!("sha256:{}", digest(canonical_body(body)?)))
}
fn digest_material(draft: &EventDraft, body_digest: &str) -> Value {
    json!({
        "schema": JOURNAL_SCHEMA,
        "id": &draft.id,
        "type": draft.event_type.as_str(),
        "entityId": &draft.entity_id,
        "revision": draft.revision,
        "parent": &draft.parent,
        "idempotencyKey": &draft.idempotency_key,
        "bodyDigest": body_digest,
        "body": &draft.body,
    })
}
fn draft_event_digest(draft: &EventDraft, body_digest: &str) -> Result<String> {
    let material = digest_material(draft, body_digest);
    let canonical = xcb_core::protocol::canonical_json(&material)
        .map_err(|_| Error::Conflict("event digest material is not canonical JSON"))?;
    Ok(format!("sha256:{}", digest(canonical)))
}
fn sql_u64(value: u64, what: &'static str) -> Result<i64> {
    i64::try_from(value).map_err(|_| Error::Conflict(what))
}
fn next_sequence(tx: &Transaction<'_>) -> Result<u64> {
    let value: i64 = tx.query_row(
        "SELECT COALESCE(MAX(sequence),0)+1 FROM journal_events",
        [],
        |row| row.get(0),
    )?;
    u64::try_from(value).map_err(|_| Error::Conflict("event sequence is invalid"))
}
fn validate_causality(
    tx: &Transaction<'_>,
    head: Option<&JournalEvent>,
    parent: Option<&CausalParent>,
    revision: u64,
) -> Result<()> {
    if let Some(head) = head {
        if revision < head.revision {
            return Err(Error::Conflict("event revision is stale"));
        }
        let parent = parent.ok_or(Error::Conflict("event causal parent is missing"))?;
        if parent.id != head.id || parent.digest != head.event_digest {
            return Err(Error::Conflict("event causal parent is not the local head"));
        }
    } else if let Some(parent) = parent {
        let existing: Option<String> = tx
            .query_row(
                "SELECT event_digest FROM journal_events WHERE id=?1",
                [parent.id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if existing.as_deref() != Some(parent.digest.as_str()) {
            return Err(Error::Conflict("event causal parent is not present"));
        }
    }
    Ok(())
}
fn find_head(db: &Connection, entity_id: &str) -> Result<Option<JournalEvent>> {
    let id: Option<String> = db
        .query_row(
            "SELECT event_id FROM journal_heads WHERE entity_id=?1",
            [entity_id],
            |row| row.get(0),
        )
        .optional()?;
    id.map(|id| find_event_by_id(db, &id)?.ok_or(Error::Conflict("event head row is missing")))
        .transpose()
}
fn find_event_by_id(db: &Connection, id: &str) -> Result<Option<JournalEvent>> {
    let row = db.query_row("SELECT sequence,id,event_type,entity_id,revision,parent_id,parent_digest,idempotency_key,body_digest,event_digest,body,occurred_at_ms FROM journal_events WHERE id=?1", [id], load_row).optional()?;
    row.map(decode_row).transpose()
}
fn find_event_by_key(db: &Connection, key: &str) -> Result<Option<JournalEvent>> {
    let row = db.query_row("SELECT sequence,id,event_type,entity_id,revision,parent_id,parent_digest,idempotency_key,body_digest,event_digest,body,occurred_at_ms FROM journal_events WHERE idempotency_key=?1", [key], load_row).optional()?;
    row.map(decode_row).transpose()
}
type EventRow = (
    i64,
    String,
    String,
    String,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    String,
    String,
    i64,
);
fn load_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
    ))
}
fn decode_row(row: EventRow) -> Result<JournalEvent> {
    let (
        sequence,
        id,
        event_type,
        entity_id,
        revision,
        parent_id,
        parent_digest,
        idempotency_key,
        body_digest,
        event_digest,
        body,
        occurred_at_ms,
    ) = row;
    let parent = match (parent_id, parent_digest) {
        (None, None) => None,
        (Some(id), Some(digest)) => Some(CausalParent { id, digest }),
        _ => return Err(Error::Conflict("event causal fields are incomplete")),
    };
    let event = JournalEvent {
        sequence: u64::try_from(sequence)
            .map_err(|_| Error::Conflict("event sequence is invalid"))?,
        id,
        event_type: EventType::parse(&event_type)?,
        entity_id,
        revision: u64::try_from(revision)
            .map_err(|_| Error::Conflict("event revision is invalid"))?,
        parent,
        idempotency_key,
        body_digest,
        event_digest,
        body: serde_json::from_str(&body)?,
        occurred_at_ms: u64::try_from(occurred_at_ms)
            .map_err(|_| Error::Conflict("event timestamp is invalid"))?,
    };
    event.validate_integrity()?;
    Ok(event)
}
fn validate_limit(limit: usize) -> Result<()> {
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(xcb_core::Error::Limit("event page").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    fn fixture() -> (tempfile::TempDir, EventJournal) {
        let dir = tempdir().expect("tempdir");
        let journal = EventJournal::open_in(dir.path()).expect("journal");
        (dir, journal)
    }
    fn state(entity: &str, revision: u64, value: &str, parent: Option<CausalParent>) -> EventDraft {
        EventDraft::state(entity, revision, json!({"value": value}), parent).expect("state")
    }
    #[test]
    fn duplicate_is_idempotent_and_body_change_conflicts() {
        let (_dir, journal) = fixture();
        let first = state("task_1", 1, "one", None)
            .with_idempotency_key("idem_1")
            .expect("key");
        let id = first.id.clone();
        assert!(
            !journal
                .append(first.clone())
                .expect("append")
                .is_duplicate()
        );
        let duplicate = journal.append(first).expect("duplicate");
        assert!(duplicate.is_duplicate());
        assert_eq!(duplicate.event().id, id);
        let changed = state("task_1", 1, "two", None)
            .with_idempotency_key("idem_1")
            .expect("key");
        assert!(matches!(journal.append(changed), Err(Error::Conflict(_))));
        let changed_key = state("task_1", 1, "two", None)
            .with_idempotency_key("idem_2")
            .expect("key");
        assert!(matches!(
            journal.append(changed_key),
            Err(Error::Conflict(_))
        ));
        assert_eq!(
            journal
                .read_page(None, None, 16)
                .expect("read")
                .events
                .len(),
            1
        );
    }
    #[test]
    fn causal_parent_replay_and_receipt_projection() {
        let (_dir, journal) = fixture();
        let first = journal
            .append(state("task_2", 1, "one", None))
            .expect("first")
            .event()
            .clone();
        let receipt = journal
            .append(
                EventDraft::receipt(
                    "task_2",
                    1,
                    "rcpt_1",
                    json!({"ok": true}),
                    Some(first.causal_parent()),
                )
                .expect("receipt"),
            )
            .expect("receipt")
            .event()
            .clone();
        journal
            .append(state("task_2", 2, "two", Some(receipt.causal_parent())))
            .expect("next");
        let replay = journal.replay("task_2", 16).expect("replay");
        assert_eq!(replay.state, Some(json!({"value": "two"})));
        assert_eq!(replay.state_revision, Some(2));
        assert_eq!(replay.receipts.len(), 1);
        assert!(
            replay.head.as_ref().is_some_and(
                |head| head.id.starts_with("evt_") && head.digest.starts_with("sha256:")
            )
        );
        let bad = state(
            "task_2",
            3,
            "three",
            Some(CausalParent {
                id: receipt.id,
                digest: format!("sha256:{}", "0".repeat(64)),
            }),
        );
        assert!(matches!(journal.append(bad), Err(Error::Conflict(_))));
    }
    #[test]
    fn paging_restart_and_bounds_are_deterministic() {
        let (dir, journal) = fixture();
        let mut parent = None;
        for revision in 1..=5 {
            let event = journal
                .append(state("task_3", revision, &revision.to_string(), parent))
                .expect("append")
                .event()
                .clone();
            parent = Some(event.causal_parent());
        }
        let first = journal.read_page(Some("task_3"), None, 2).expect("first");
        assert_eq!(first.events.len(), 2);
        assert!(first.has_more);
        let second = journal
            .read_page(Some("task_3"), first.next_cursor.as_deref(), 2)
            .expect("second");
        let third = journal
            .read_page(Some("task_3"), second.next_cursor.as_deref(), 2)
            .expect("third");
        assert_eq!(second.events.len(), 2);
        assert_eq!(third.events.len(), 1);
        assert!(!third.has_more);
        assert!(matches!(
            journal.read_page(None, None, MAX_PAGE_SIZE + 1),
            Err(Error::Core(xcb_core::Error::Limit("event page")))
        ));
        drop(journal);
        let reopened = EventJournal::open_in(dir.path()).expect("reopen");
        assert_eq!(reopened.verify(16).expect("verify"), 5);
        assert_eq!(
            reopened.replay("task_3", 16).expect("replay").events.len(),
            5
        );
    }
    #[test]
    fn fault_and_restart_keep_the_local_journal_authoritative() {
        let (dir, journal) = fixture();
        journal
            .append(EventDraft::fault("task_4", 1, "evidence_hold", "retain", None).expect("fault"))
            .expect("append");
        drop(journal);
        // A malformed row simulates a torn/corrupt local write. Reopen never
        // repairs or skips it; readers retain the evidence and fail closed.
        let connection = Connection::open(dir.path().join("events.sqlite")).expect("db");
        connection.execute("INSERT INTO journal_events(sequence,id,event_type,entity_id,revision,parent_id,parent_digest,idempotency_key,body_digest,event_digest,body,occurred_at_ms) VALUES(2,'evt_bad','fault','task_4',1,NULL,NULL,NULL,'sha256:bad','sha256:bad','{}',0)", []).expect("fault insert");
        drop(connection);
        let reopened = EventJournal::open_in(dir.path()).expect("reopen");
        assert!(matches!(
            reopened.read_page(None, None, 8),
            Err(Error::Json(_)) | Err(Error::Conflict(_))
        ));
    }
    /// Retained minimized history used as a deterministic shrink target for
    /// stateful crash/restart runs.
    #[test]
    fn retained_shrunk_history_replays_after_fault() {
        let (_dir, journal) = fixture();
        let first = journal
            .append(state("task_5", 1, "open", None))
            .expect("state")
            .event()
            .clone();
        let receipt = journal
            .append(
                EventDraft::receipt(
                    "task_5",
                    1,
                    "rcpt_5",
                    json!({"ok": true}),
                    Some(first.causal_parent()),
                )
                .expect("receipt"),
            )
            .expect("receipt")
            .event()
            .clone();
        let second = journal
            .append(state("task_5", 2, "closed", Some(receipt.causal_parent())))
            .expect("state")
            .event()
            .clone();
        journal
            .append(
                EventDraft::fault(
                    "task_5",
                    2,
                    "replay_hold",
                    "shrunk history",
                    Some(second.causal_parent()),
                )
                .expect("fault"),
            )
            .expect("fault");
        let replay = journal.replay("task_5", 8).expect("replay");
        assert_eq!(replay.events.len(), 4);
        assert_eq!(replay.faults, vec!["replay_hold"]);
        assert_eq!(replay.state, Some(json!({"value": "closed"})));
    }
}
