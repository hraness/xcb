//! Durable ALGAL suspension. The interpreter never dispatches a provider: a
//! joined slice publishes an ordinary child and its checkpoint atomically.
use super::*;
use crate::managed_program::{
    AdmittedProgram, MAX_CHECKPOINT_BYTES, MAX_MANAGED_CALLS, MAX_PROMPT_BYTES, MAX_SUMMARY_BYTES,
    ProgramCall, ProgramCallResult, ProgramSlice, ProgramSliceOutcome,
};
use crate::workspace_infer::BindingOrigin;
use algal::agent_context::{
    AgentContextEntryInput, AgentContextHost, AgentContextKind, AgentContextRef, put_agent_context,
};

const MAX_CONTEXT_SOURCE_BYTES: usize = 32 * 1024;
const MAX_CALL_RECORD_BYTES: usize = 128 * 1024;
const MAX_HISTORY_INPUT_BYTES: usize = 256 * 1024;
const CONTEXT_LIMITS: &str = r#"{"maxReadBytes":32768,"maxScanBytes":32768,"maxSearchResults":32}"#;
const HISTORY_LIMITS: &str = r#"{"maxReadBytes":32768,"maxScanBytes":262144,"maxOutputBytes":32768,"maxNodes":256,"maxWork":4194304,"maxSearchResults":16}"#;
const PROGRAM_HISTORY_CONTRACT: &str = "xcb.program-history.v1";

/// Scope fields are bounded application identifiers; a task or workspace
/// identity joins them through its digest rather than raw bytes.
fn scope_id(value: &str) -> String {
    format!("h{}", &digest(value.as_bytes())[..63])
}

/// The versioned `xcb.program-history.v1` read input carried by
/// `xcb_context_query` op `history`. Fields beyond the contract and view are
/// used only by the view they name.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProgramHistoryQuery {
    contract: String,
    view: String,
    #[serde(default)]
    recent_leaves: Option<usize>,
    #[serde(default)]
    derivatives: Option<Value>,
    #[serde(default)]
    node: Option<String>,
    #[serde(default)]
    source_index: Option<usize>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    max_results: Option<usize>,
    #[serde(default)]
    max_scan_bytes: Option<usize>,
    #[serde(default)]
    limits: Option<Value>,
}

/// Exact sources live inside the existing digest-protected call record. The
/// reconstructed ALGAL CAS is read-only to the worker and shared identities are
/// verified again after a restart. No provider transcript or account is copied.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProgramContext {
    reference: AgentContextRef,
    entries: Vec<AgentContextEntryInput>,
}
impl ProgramContext {
    fn store(&self) -> Result<algal::store::Store> {
        if self.entries.len() > 5
            || self
                .entries
                .iter()
                .map(|entry| entry.text.len())
                .sum::<usize>()
                > MAX_CONTEXT_SOURCE_BYTES
        {
            return Err(Error::Conflict("program context source bound exceeded"));
        }
        let mut store = algal::store::Store::default();
        let snapshot = put_agent_context(&mut store, &self.entries)
            .map_err(|_| Error::Conflict("program context sources changed"))?;
        let reference = AgentContextHost::new(&store)
            .grant(
                &snapshot,
                None,
                Some(&serde_json::from_str(CONTEXT_LIMITS)?),
            )
            .map_err(|_| Error::Conflict("program context permission changed"))?;
        if reference != self.reference {
            return Err(Error::Conflict("program context snapshot changed"));
        }
        Ok(store)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramChild {
    pub parent: Id,
    pub call: u8,
    pub request_digest: String,
    pub generation: Id,
    pub required_provider: Option<Provider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<AgentContextRef>,
}
impl ProgramChild {
    pub(super) fn validate(&self) -> Result<()> {
        if self.call == 0 || self.call > MAX_MANAGED_CALLS || !valid_digest(&self.request_digest) {
            return Err(Error::Conflict("invalid managed program child"));
        }
        if let Some(reference) = &self.context {
            algal::agent_context::parse_agent_context_ref(&serde_json::to_value(reference)?)
                .map_err(|_| Error::Conflict("invalid managed program context"))?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgramStatus {
    pub parent: Id,
    pub phase: String,
    pub calls: u8,
    pub max_calls: u8,
    pub child: Option<Id>,
    pub child_status: Option<String>,
    pub receipt: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Execution {
    parent: Id,
    checkpoint: Value,
    receipt: String,
    calls: u8,
    waiting: Option<u8>,
    response: Option<ProgramCallResult>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Call {
    parent: Id,
    index: u8,
    request: ProgramCall,
    child: Id,
    result: Option<Settlement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context: Option<ProgramContext>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Settlement {
    pub(super) revision: u64,
    pub(super) receipt: String,
    pub(super) summary: String,
    pub(super) summary_digest: String,
    pub(super) session: Option<Id>,
    pub(super) message_count_before: usize,
    pub(super) outcome_digest: Option<String>,
}
pub(super) struct ChildPublication {
    task: ManagedTask,
    receipt: String,
    user: Message,
    ack: Message,
    policy: ProjectPolicy,
    context: ProgramContext,
}
pub(super) enum Change {
    Publish {
        previous: Option<Execution>,
        execution: Execution,
        call: Call,
        child: Box<ChildPublication>,
    },
    Resume {
        previous: Execution,
        execution: Execution,
        call: Call,
        child: Box<ManagedTask>,
    },
    Complete {
        previous: Option<Execution>,
        execution: Execution,
    },
}
fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(xcb_core::hex64)
}
pub(super) fn migrate(db: &mut Connection) -> Result<()> {
    let version: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version >= 5 {
        return Ok(());
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS program_executions(parent TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,digest TEXT NOT NULL,payload TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS program_calls(parent TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,call_index INTEGER NOT NULL,child TEXT NOT NULL UNIQUE REFERENCES tasks(id),digest TEXT NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(parent,call_index));
        PRAGMA user_version=5;")?;
    tx.commit()?;
    Ok(())
}
fn read_execution(db: &Connection, parent: &Id) -> Result<Option<Execution>> {
    let row: Option<(String, String)> = db
        .query_row(
            "SELECT digest,substr(payload,1,600001) FROM program_executions WHERE parent=?1",
            [parent.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    row.map(|(hash, payload)| {
        if payload.len() > 600_000 || digest(&payload) != hash {
            return Err(Error::Conflict("program checkpoint integrity failed"));
        }
        let execution: Execution = decode(&payload)?;
        if execution.parent != *parent
            || !valid_digest(&execution.receipt)
            || execution.calls > MAX_MANAGED_CALLS
            || execution
                .waiting
                .is_some_and(|index| index == 0 || index != execution.calls)
            || (execution.waiting.is_some() && execution.response.is_some())
            || serde_json::to_vec(&execution.checkpoint)?.len() > MAX_CHECKPOINT_BYTES
        {
            return Err(Error::Conflict("invalid program checkpoint"));
        }
        Ok(execution)
    })
    .transpose()
}
fn read_call(db: &Connection, parent: &Id, index: u8) -> Result<Call> {
    let (child, hash, payload): (String, String, String) = db.query_row(
        "SELECT child,digest,CAST(substr(CAST(payload AS BLOB),1,?3) AS TEXT) FROM program_calls WHERE parent=?1 AND call_index=?2",
        params![parent.as_str(), index, (MAX_CALL_RECORD_BYTES + 1) as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).optional()?.ok_or(Error::Conflict("program call evidence is missing"))?;
    if payload.len() > MAX_CALL_RECORD_BYTES || digest(&payload) != hash {
        return Err(Error::Conflict("program call integrity failed"));
    }
    let call: Call = decode(&payload)?;
    if call.request.request.is_some() {
        crate::managed_program::retained_request(&call.request)?;
    }
    if call.parent != *parent
        || call.index != index
        || call.child.as_str() != child
        || !valid_digest(&call.request.digest)
        || call.request.prompt.len() > MAX_PROMPT_BYTES
        || call.result.as_ref().is_some_and(|result| {
            result.revision == 0
                || !valid_digest(&result.receipt)
                || result.summary.len() > MAX_SUMMARY_BYTES
                || digest(&result.summary) != result.summary_digest
                || result
                    .outcome_digest
                    .as_ref()
                    .is_some_and(|hash| !xcb_core::hex64(hash))
        })
    {
        return Err(Error::Conflict("invalid program call evidence"));
    }
    if let Some(context) = &call.context {
        // Read only the parent's immutable digest fields; its hidden source
        // text must never become part of this child's reconstructed context.
        let (manifest, inputs): (String, String) = db.query_row(
            "SELECT substr(json_extract(payload,'$.program.manifestDigest'),1,72),substr(json_extract(payload,'$.program.inputsDigest'),1,72) FROM tasks WHERE id=?1",
            [parent.as_str()], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if context.entries != context_entries(parent, &call.request, index, &manifest, &inputs)? {
            return Err(Error::Conflict(
                "program context differs from exact effect source",
            ));
        }
        context.store()?;
    }
    Ok(call)
}
fn write_execution(tx: &Transaction<'_>, execution: &Execution) -> Result<()> {
    let payload = serde_json::to_string(execution)?;
    if payload.len() > 600_000
        || serde_json::to_vec(&execution.checkpoint)?.len() > MAX_CHECKPOINT_BYTES
    {
        return Err(xcb_core::Error::Limit("program checkpoint").into());
    }
    tx.execute("INSERT INTO program_executions(parent,digest,payload) VALUES(?1,?2,?3) ON CONFLICT(parent) DO UPDATE SET digest=excluded.digest,payload=excluded.payload",
        params![execution.parent.as_str(), digest(&payload), payload])?;
    Ok(())
}
fn write_call(tx: &Transaction<'_>, call: &Call) -> Result<()> {
    let payload = serde_json::to_string(call)?;
    bounded_text(&payload, MAX_CALL_RECORD_BYTES)?;
    tx.execute("INSERT INTO program_calls(parent,call_index,child,digest,payload) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(parent,call_index) DO UPDATE SET digest=excluded.digest,payload=excluded.payload",
        params![call.parent.as_str(), call.index, call.child.as_str(), digest(&payload), payload])?;
    Ok(())
}
fn same<T: Serialize>(a: &T, b: &T) -> Result<bool> {
    Ok(serde_json::to_value(a)? == serde_json::to_value(b)?)
}
fn check_previous(tx: &Connection, parent: &Id, previous: &Option<Execution>) -> Result<()> {
    if !same(&read_execution(tx, parent)?, previous)? {
        return Err(Error::Conflict("program checkpoint changed"));
    }
    Ok(())
}
/// The current grant for `workspace`: enabled, unexpired, of `generation`
/// when given, and with budget left when `budget` is set.
pub(super) fn require_grant(
    db: &Connection,
    workspace: &str,
    generation: Option<&Id>,
    now: u64,
    budget: bool,
) -> Result<ProjectPolicy> {
    let policy = project::policy_from(db, workspace)?.ok_or(Error::Conflict(
        "project authority is required for managed programs",
    ))?;
    if !policy.enabled
        || policy.expires_at_ms <= now
        || generation.is_some_and(|id| *id != policy.generation)
    {
        return Err(Error::Conflict(
            "project authority is paused, expired, or replaced",
        ));
    }
    if budget && policy.admitted_tasks >= policy.max_tasks {
        return Err(Error::Conflict(
            "project authority task budget is exhausted",
        ));
    }
    Ok(policy)
}
pub(super) fn check_creation(tx: &Connection, task: &ManagedTask) -> Result<()> {
    if task.program.as_ref().is_some_and(|p| p.managed_calls > 0) {
        let generation = task.program_generation.as_ref().ok_or(Error::Conflict(
            "project authority is required for managed programs",
        ))?;
        require_grant(tx, &task.workspace, Some(generation), now_ms(), true)?;
        no_other_work(tx, task)?;
    }
    Ok(())
}
pub(super) fn check_dispatch(db: &Connection, task: &ManagedTask, now: u64) -> Result<()> {
    if task.program.as_ref().is_some_and(|p| p.managed_calls > 0) {
        let generation = task
            .program_generation
            .as_ref()
            .ok_or(Error::Conflict("project authority is missing"))?;
        require_grant(db, &task.workspace, Some(generation), now, false)?;
        if task.program_waiting {
            return Err(Error::Conflict("program waits for its linked child"));
        }
        if task.detail == "project authority task budget is exhausted" {
            require_grant(db, &task.workspace, Some(generation), now, true)?;
        }
        if task.detail == "project hourly start limit reached" {
            let policy = require_grant(db, &task.workspace, Some(generation), now, false)?;
            project::check_admission_window(db, &policy, now)?;
        }
        if task.detail == "program waits for other project work to settle" {
            no_other_work(db, task)?;
        }
    }
    if let Some(link) = &task.daemon_child {
        daemon::check_child_dispatch(db, task, link, now)?;
    }
    if let Some(link) = &task.program_child {
        let policy = require_grant(db, &task.workspace, Some(&link.generation), now, false)?;
        let parent =
            task_from(db, &link.parent)?.ok_or(Error::Conflict("program parent is missing"))?;
        let execution = read_execution(db, &parent.id)?
            .ok_or(Error::Conflict("program checkpoint is missing"))?;
        let call = read_call(db, &parent.id, link.call)?;
        if parent.cancel_requested
            || parent.state.terminal()
            || !parent.program_waiting
            || parent.conversation != task.conversation
            || parent.workspace != task.workspace
            || execution.waiting != Some(link.call)
            || call.child != task.id
            || call.result.is_some()
            || call.request.digest != link.request_digest
            || call.context.as_ref().map(|context| &context.reference) != link.context.as_ref()
            || policy.required_provider != link.required_provider
            || (link.required_provider.is_some()
                && (!task.provider_required || task.provider_preference != link.required_provider))
        {
            return Err(Error::Conflict("program child authority changed"));
        }
    }
    Ok(())
}
/// No other undeferred work is outstanding in the program's workspace.
fn no_other_work(db: &Connection, parent: &ManagedTask) -> Result<()> {
    let mut query = db.prepare("SELECT id,payload FROM tasks WHERE workspace=?1 AND id<>?2 AND state IN ('queued','running','needs_input','uncertain')")?;
    for row in query.query_map(
        params![parent.workspace.as_str(), parent.id.as_str()],
        |row| row.get::<_, String>(1),
    )? {
        let task: ManagedTask = decode(&row?)?;
        task.validate()?;
        if !task.deferred {
            return Err(Error::Conflict(
                "program waits for other project work to settle",
            ));
        }
    }
    Ok(())
}
pub(super) fn transition(
    tx: &Transaction<'_>,
    expected: &ManagedTask,
    next: &ManagedTask,
    change: Option<&Change>,
) -> Result<()> {
    let Some(change) = change else {
        return Ok(());
    };
    match change {
        Change::Publish {
            previous,
            execution,
            call,
            child,
        } => {
            check_previous(tx, &expected.id, previous)?;
            if expected.cancel_requested
                || expected.state != TaskState::Running
                || !next.program_waiting
                || call.parent != expected.id
                || call.child != child.task.id
                || call.index != previous.as_ref().map_or(1, |e| e.calls + 1)
                || call.index > expected.program.as_ref().map_or(0, |p| p.managed_calls)
                || call.context.as_ref().map(|context| &context.reference)
                    != child
                        .task
                        .program_child
                        .as_ref()
                        .and_then(|link| link.context.as_ref())
            {
                return Err(Error::Conflict("program call publication changed"));
            }
            let policy = require_grant(
                tx,
                &expected.workspace,
                expected.program_generation.as_ref(),
                now_ms(),
                true,
            )?;
            if !same(&policy, &child.policy)? {
                return Err(Error::Conflict(
                    "project authority changed before program publication",
                ));
            }
            project::check_admission_window(tx, &policy, now_ms())?;
            no_other_work(tx, expected)?;
            if task_from(tx, &child.task.id)?.is_some() {
                return Err(Error::Conflict("program child identity already exists"));
            }
            let count: i64 = tx.query_row("SELECT count(*) FROM tasks", [], |row| row.get(0))?;
            let active: i64 = tx.query_row(
                "SELECT count(*) FROM tasks WHERE state IN ('queued','running','needs_input')",
                [],
                |row| row.get(0),
            )?;
            if count >= MAX_TASKS || active >= MAX_NONTERMINAL_TASKS {
                return Err(xcb_core::Error::Limit("managed program tasks").into());
            }
            let t = &child.task;
            tx.execute("INSERT INTO tasks(id,operation,source_message,conversation,state,revision,updated_at,payload) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![t.id.as_str(),t.operation.as_str(),t.source_message.as_str(),t.conversation.as_str(),t.state.as_str(),sql(t.revision)?,sql(t.updated_at_ms)?,serde_json::to_string(t)?])?;
            tx.execute(
                "INSERT INTO receipts(digest,task,revision,payload) VALUES(?1,?2,?3,?4)",
                params![
                    t.last_receipt,
                    t.id.as_str(),
                    sql(t.revision)?,
                    child.receipt
                ],
            )?;
            ManagedStore::append_message_tx(tx, &child.user, &t.conversation, Some(&t.id))?;
            ManagedStore::append_message_tx(tx, &child.ack, &t.conversation, Some(&t.id))?;
            let mut policy = policy;
            policy.admitted_tasks += 1;
            policy.revision += 1;
            project::write_policy(tx, &policy)?;
            write_call(tx, call)?;
            write_execution(tx, execution)?;
        }
        Change::Resume {
            previous,
            execution,
            call,
            child,
        } => {
            check_previous(tx, &expected.id, &Some(previous.clone()))?;
            let current =
                task_from(tx, &child.id)?.ok_or(Error::Conflict("program child disappeared"))?;
            let old = read_call(tx, &expected.id, call.index)?;
            if !same(&current, child.as_ref())?
                || old.result.is_some()
                || expected.cancel_requested
                || !expected.program_waiting
                || previous.waiting != Some(call.index)
                || old.child != child.id
                || call
                    .result
                    .as_ref()
                    .is_none_or(|r| r.revision != child.revision || r.receipt != child.last_receipt)
            {
                return Err(Error::Conflict("program child settlement changed"));
            }
            require_grant(
                tx,
                &expected.workspace,
                expected.program_generation.as_ref(),
                now_ms(),
                false,
            )?;
            write_call(tx, call)?;
            write_execution(tx, execution)?;
        }
        Change::Complete {
            previous,
            execution,
        } => {
            check_previous(tx, &expected.id, previous)?;
            if expected.cancel_requested || expected.program_waiting || execution.waiting.is_some()
            {
                return Err(Error::Conflict("program completion changed"));
            }
            write_execution(tx, execution)?;
        }
    }
    Ok(())
}
pub(super) fn retain_task(tx: &Transaction<'_>, id: &str) -> Result<()> {
    tx.execute("DELETE FROM program_calls WHERE parent=?1", [id])?;
    tx.execute("DELETE FROM program_executions WHERE parent=?1", [id])?;
    Ok(())
}

fn context_entries(
    parent: &Id,
    call: &ProgramCall,
    index: u8,
    manifest_digest: &str,
    inputs_digest: &str,
) -> Result<Vec<AgentContextEntryInput>> {
    if !valid_digest(manifest_digest) || !valid_digest(inputs_digest) {
        return Err(Error::Conflict("program context lineage changed"));
    }
    let canonical = |value: &Value| {
        algal::canonical::canonical(value)
            .map_err(|_| Error::Conflict("program context source is invalid"))
    };
    let request = crate::managed_program::retained_request(call)?;
    // A project/task grant does not widen an ALGAL cell's declared view.
    // Never add the full parent program, hidden inputs or sibling reports.
    Ok(vec![
        AgentContextEntryInput {
            kind: AgentContextKind::Instruction,
            label: "effect-instructions".into(),
            text: request["prompt"]
                .as_str()
                .ok_or(Error::Conflict("program effect instruction missing"))?
                .to_owned(),
        },
        AgentContextEntryInput {
            kind: AgentContextKind::Input,
            label: "effect-context".into(),
            text: canonical(&request["context"])?,
        },
        AgentContextEntryInput {
            kind: AgentContextKind::Observation,
            label: "effect-request".into(),
            text: canonical(request)?,
        },
        AgentContextEntryInput {
            kind: AgentContextKind::Instruction,
            label: "current-call".into(),
            text: call.prompt.clone(),
        },
        AgentContextEntryInput {
            kind: AgentContextKind::Observation,
            label: "current-lineage".into(),
            text: canonical(
                &json!({"parent":parent,"call":index,"cellId":request["cellId"],"requestDigest":call.digest,"manifestDigest":manifest_digest,"inputsDigest":inputs_digest}),
            )?,
        },
    ])
}

impl ManagedStore {
    fn capture_program_context(
        &self,
        parent: &ManagedTask,
        call: &ProgramCall,
        index: u8,
    ) -> Result<ProgramContext> {
        let program = parent
            .program
            .as_ref()
            .ok_or(Error::Conflict("program source missing"))?;
        program.verify()?;
        let entries = context_entries(
            &parent.id,
            call,
            index,
            &program.manifest_digest,
            &program.inputs_digest,
        )?;
        let mut store = algal::store::Store::default();
        let snapshot = put_agent_context(&mut store, &entries)
            .map_err(|_| Error::Conflict("program context source bound exceeded"))?;
        let reference = AgentContextHost::new(&store)
            .grant(
                &snapshot,
                None,
                Some(&serde_json::from_str(CONTEXT_LIMITS)?),
            )
            .map_err(|_| Error::Conflict("program context permission failed"))?;
        let context = ProgramContext { reference, entries };
        context.store()?;
        Ok(context)
    }

    /// Called only after worker_call derives the current task from its active
    /// session. The request cannot name another task, call, snapshot or grant.
    pub(super) fn program_context_query(
        &self,
        source: &ManagedTask,
        input: &Value,
    ) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(tag = "op", rename_all = "lowercase", deny_unknown_fields)]
        enum Query {
            Inspect {
                #[serde(default)]
                offset: usize,
                #[serde(default = "page_limit")]
                limit: usize,
            },
            Read {
                index: usize,
            },
            Slice {
                index: usize,
                #[serde(rename = "startByte")]
                start_byte: usize,
                #[serde(rename = "endByte")]
                end_byte: usize,
            },
            Search {
                query: String,
                #[serde(default, rename = "maxResults")]
                max_results: Option<usize>,
                #[serde(default, rename = "maxScanBytes")]
                max_scan_bytes: Option<usize>,
            },
            History {
                history: Box<ProgramHistoryQuery>,
            },
        }
        fn page_limit() -> usize {
            16
        }
        if serde_json::to_vec(input)?.len()
            > if input["op"] == "history" {
                MAX_HISTORY_INPUT_BYTES
            } else {
                8192
            }
        {
            return Err(xcb_core::Error::Limit("context query bytes").into());
        }
        for key in ["maxResults", "maxScanBytes"] {
            if input.get(key).is_some_and(|value| value.as_u64().is_none()) {
                return Err(xcb_core::Error::Invalid("context query limit").into());
            }
        }
        let query: Query = serde_json::from_value(input.clone())?;
        let link = source.program_child.as_ref().ok_or(Error::Unavailable(
            "exact context requires a managed program child",
        ))?;
        let reference = link.context.as_ref().ok_or(Error::Unavailable(
            "this older program child has no retained exact context",
        ))?;
        let db = self.db()?;
        check_dispatch(&db, source, now_ms())?;
        let call = read_call(&db, &link.parent, link.call)?;
        if call.child != source.id || call.request.digest != link.request_digest {
            return Err(Error::Conflict("program context task ownership changed"));
        }
        let context = call
            .context
            .as_ref()
            .ok_or(Error::Conflict("program context record missing"))?;
        if &context.reference != reference {
            return Err(Error::Conflict("program context reference changed"));
        }
        let store = context.store()?;
        let mut host = AgentContextHost::new(&store);
        let granted = host
            .grant(
                &reference.snapshot,
                None,
                Some(&serde_json::from_str(CONTEXT_LIMITS)?),
            )
            .map_err(|_| Error::Conflict("program context permission failed"))?;
        if &granted != reference {
            return Err(Error::Conflict("program context scope changed"));
        }
        let context_error = |_| Error::Conflict("context query exceeds its scope or limits");
        let result = match query {
            Query::Inspect { offset, limit } => {
                if offset > 5 || !(1..=32).contains(&limit) {
                    return Err(xcb_core::Error::Invalid("context catalog page").into());
                }
                let catalog = host.inspect(&granted).map_err(context_error)?;
                let total = catalog.entries.len();
                json!({"snapshot":catalog.snapshot,"entries":catalog.entries.into_iter().skip(offset).take(limit).collect::<Vec<_>>(),"offset":offset,"totalEntries":total,"nextOffset":(offset + limit < total).then_some(offset + limit)})
            }
            Query::Read { index } => {
                serde_json::to_value(host.read(&granted, index).map_err(context_error)?)?
            }
            Query::Slice {
                index,
                start_byte,
                end_byte,
            } => json!(
                host.slice(&granted, index, start_byte, end_byte)
                    .map_err(context_error)?
            ),
            Query::Search {
                query,
                max_results,
                max_scan_bytes,
            } => {
                if query.len() > 4096 {
                    return Err(xcb_core::Error::Limit("context query text").into());
                }
                let mut options = json!({"query":query});
                if let Some(limit) = max_results {
                    options["maxResults"] = json!(limit);
                }
                if let Some(limit) = max_scan_bytes {
                    options["maxScanBytes"] = json!(limit);
                }
                serde_json::to_value(host.search(&granted, &options).map_err(context_error)?)?
            }
            Query::History { history } => {
                self.program_history_query(&db, source, link, &call, &history)?
            }
        };
        bounded_text(&serde_json::to_string(&result)?, xcb_core::MAX_TEXT_BYTES)?;
        Ok(result)
    }

    /// Progressive view over the program's retained call records, scoped to
    /// this child as the audience (`xcb.program-history.v1`). Every leaf keeps
    /// its original call index, child task identity and settled state; a call's
    /// report body is present only when that call was declared an input of the
    /// audience cell, so workspace membership never discloses hidden inputs or
    /// sibling reports. The history is rebuilt from the digest-verified call
    /// records on each query; the store is in-memory and reads write nothing.
    /// Summaries arrive only through caller-supplied derivative generations,
    /// which the shared contract validates against this captured history.
    fn program_history_query(
        &self,
        db: &Connection,
        source: &ManagedTask,
        link: &ProgramChild,
        own_call: &Call,
        input: &ProgramHistoryQuery,
    ) -> Result<Value> {
        if input.contract != PROGRAM_HISTORY_CONTRACT {
            return Err(Error::Unavailable("unsupported program history contract"));
        }
        let parent =
            task_from(db, &link.parent)?.ok_or(Error::Conflict("program parent is missing"))?;
        let program = parent
            .program
            .as_ref()
            .ok_or(Error::Conflict("program source missing"))?;
        program.verify()?;
        let execution = read_execution(db, &parent.id)?
            .ok_or(Error::Unavailable("program has no retained calls"))?;
        if execution.calls == 0 {
            return Err(Error::Unavailable("program has no retained calls"));
        }
        let own_request = crate::managed_program::retained_request(&own_call.request)?;
        let own_cell = own_request["cellId"]
            .as_str()
            .ok_or(Error::Conflict("program call cell is missing"))?
            .to_owned();
        let mut declared = BTreeSet::from([own_cell.clone()]);
        if let Some(edges) = program.manifest["edges"].as_array() {
            for edge in edges.iter().take(128) {
                if edge["to"]["cell"].as_str() == Some(own_cell.as_str())
                    && let Some(cell) = edge["from"]["cell"].as_str()
                {
                    declared.insert(cell.to_owned());
                }
            }
        }
        let mut store = algal::store::Store::default();
        let mut entries = Vec::new();
        let mut sources = Vec::new();
        for index in 1..=execution.calls {
            let call = read_call(db, &parent.id, index)?;
            let cell = call
                .request
                .request
                .as_ref()
                .and_then(|_| crate::managed_program::retained_request(&call.request).ok())
                .and_then(|request| request["cellId"].as_str().map(str::to_owned));
            let state = task_from(db, &call.child)?
                .map_or("unavailable", |task| task.state.as_str())
                .to_owned();
            let mut record = json!({
                "schema": "xcb.program-call-record.v1",
                "call": index,
                "cellId": cell,
                "requestDigest": call.request.digest,
                "state": state,
                "task": call.child.as_str(),
            });
            if cell.as_ref().is_some_and(|cell| declared.contains(cell)) {
                record["result"] = call.result.as_ref().map_or(Value::Null, |result| {
                    json!({
                        "outcomeDigest": result.outcome_digest,
                        "receipt": result.receipt,
                        "revision": result.revision,
                        "summary": result.summary,
                        "summaryDigest": result.summary_digest,
                    })
                });
            }
            // The event is the leaf's immutable identity: call position and
            // request digest only. The live task state lives in the leaf body,
            // so an already-published leaf cannot churn as tasks settle.
            let event = algal::canonical::digest(&json!({
                "schema": "xcb.program-call-event.v1",
                "call": index,
                "requestDigest": call.request.digest,
            }))
            .map_err(|_| Error::Conflict("program history event is invalid"))?;
            sources.push(json!({
                "sourceIndex": entries.len(),
                "position": entries.len(),
                "event": event,
            }));
            entries.push(AgentContextEntryInput {
                kind: AgentContextKind::Observation,
                label: format!("call-{index}"),
                text: algal::canonical::canonical(&record)
                    .map_err(|_| Error::Conflict("program history source is invalid"))?,
            });
        }
        let snapshot = put_agent_context(&mut store, &entries)
            .map_err(|_| Error::Conflict("program history source bound exceeded"))?;
        let scope = json!({
            "application": "xcb-managed",
            "realm": "program",
            "workspace": scope_id(&parent.workspace),
            "task": scope_id(parent.id.as_str()),
            "audience": scope_id(source.id.as_str()),
        });
        let head = algal::canonical::digest(&json!({
            "schema": "xcb.program-call-head.v1",
            "calls": execution.calls,
            "manifestDigest": program.manifest_digest,
            "parent": parent.id.as_str(),
            "receipt": execution.receipt,
        }))
        .map_err(|_| Error::Conflict("program history head is invalid"))?;
        let history = serde_json::to_value(
            algal::context_history::capture_context_history(
                &store,
                &json!({
                    "scope": scope,
                    "head": head,
                    "snapshot": snapshot,
                    "epoch": 0,
                    "firstPosition": 0,
                    "sources": sources,
                }),
            )
            .map_err(|_| Error::Conflict("program history capture failed"))?,
        )?;
        let history_id =
            algal::canonical::digest(&history).map_err(|_| Error::Conflict("program history"))?;
        let history_scope = history["scope"].clone();
        let indices = json!((0..execution.calls as usize).collect::<Vec<usize>>());
        let resolve =
            move |captured: &algal::context_history_contract::ContextHistory,
                  _principal: &str|
                  -> algal::Result<algal::context_history::ContextHistoryCurrent> {
                Ok(algal::context_history::ContextHistoryCurrent {
                    access: json!({
                        "schema": "algal.context-history-access.v1",
                        "history": history_id,
                        "scope": history_scope,
                        "head": captured.head,
                        "snapshot": captured.snapshot,
                        "revision": 0,
                        "indices": indices,
                        "state": "active",
                    }),
                    invalidated: Vec::new(),
                })
            };
        let mut host = algal::context_history::ContextHistoryHost::new(
            &store,
            &scope_id(source.id.as_str()),
            resolve,
        )
        .map_err(|_| Error::Conflict("program history host failed"))?;
        let mut config = json!({
            "recentLeaves": input.recent_leaves.unwrap_or(2),
        });
        if let Some(derivatives) = &input.derivatives {
            config["derivatives"] = derivatives.clone();
        }
        let limits: Value = serde_json::from_str(HISTORY_LIMITS)?;
        let context_error =
            |_| Error::Conflict("context history query exceeds its scope or limits");
        let reference = host
            .admit(&history, Some(&config), None, Some(&limits))
            .map_err(context_error)?;
        let requested = input.limits.as_ref();
        let result = match input.view.as_str() {
            "inspect" => host.inspect(&reference, None),
            "overview" => {
                let mut options = json!({});
                if let Some(limits) = requested {
                    options["limits"] = limits.clone();
                }
                host.overview(&reference, &options, None)
            }
            "expand" => host.expand(
                &reference,
                input
                    .node
                    .as_deref()
                    .ok_or(Error::Unavailable("program history node is required"))?,
                requested,
                None,
            ),
            "read" => host.read(
                &reference,
                input.source_index.ok_or(Error::Unavailable(
                    "program history sourceIndex is required",
                ))?,
                requested,
                None,
            ),
            "search" => {
                let query = input
                    .query
                    .as_deref()
                    .ok_or(Error::Unavailable("program history query is required"))?;
                if query.is_empty() || query.len() > 4096 {
                    return Err(xcb_core::Error::Limit("context history query text").into());
                }
                let mut options = json!({"query": query});
                if let Some(limit) = input.max_results {
                    options["maxResults"] = json!(limit);
                }
                if let Some(limit) = input.max_scan_bytes {
                    options["maxScanBytes"] = json!(limit);
                }
                host.search(&reference, &options, requested, None)
            }
            _ => return Err(Error::Unavailable("unsupported program history view")),
        }
        .map_err(context_error)?;
        bounded_text(&serde_json::to_string(&result)?, xcb_core::MAX_TEXT_BYTES)?;
        Ok(result)
    }

    /// Shim: run a program in a project view's directory.
    pub async fn enqueue_program(
        &self,
        conversation: &Id,
        operation: Id,
        title: String,
        program: AdmittedProgram,
    ) -> Result<ManagedTask> {
        self.enqueue_program_at(
            conversation,
            None,
            BindingOrigin::Cli,
            operation,
            title,
            program,
        )
        .await
    }
    /// Run a program in `workspace` (see `entry_workspace`). A thread task
    /// records `origin` in its explicit binding.
    pub async fn enqueue_program_at(
        &self,
        conversation: &Id,
        workspace: Option<&Path>,
        origin: BindingOrigin,
        operation: Id,
        title: String,
        program: AdmittedProgram,
    ) -> Result<ManagedTask> {
        habitat::validate_prompt(&title)?;
        program.verify()?;
        let workspace = self.entry_workspace(conversation, workspace)?;
        let binding = habitat::explicit_binding(conversation, origin);
        if binding.is_some() {
            self.global_thread().await?;
        }
        self.create_habitat_task(
            conversation,
            operation,
            title,
            vec![],
            Path::new(&workspace),
            habitat::CreateOptions {
                program: Some(&program),
                binding,
                ..Default::default()
            },
        )
        .await
    }
    pub fn program_status(&self, id: &Id) -> Result<Option<ProgramStatus>> {
        let db = self.db()?;
        let Some(task) = task_from(&db, id)? else {
            return Ok(None);
        };
        let parent = if let Some(link) = &task.program_child {
            task_from(&db, &link.parent)?.ok_or(Error::Conflict("program parent missing"))?
        } else {
            task
        };
        let Some(program) = &parent.program else {
            return Ok(None);
        };
        let execution = read_execution(&db, &parent.id)?;
        let call = execution
            .as_ref()
            .filter(|e| e.calls > 0)
            .map(|e| read_call(&db, &parent.id, e.calls))
            .transpose()?;
        let child = call
            .as_ref()
            .map(|call| {
                task_from(&db, &call.child)?.ok_or(Error::Conflict("program child missing"))
            })
            .transpose()?;
        Ok(Some(ProgramStatus {
            parent: parent.id.clone(),
            phase: if parent.program_waiting {
                "waiting for child".into()
            } else {
                parent.habitat_status().into()
            },
            calls: execution.as_ref().map_or(0, |e| e.calls),
            max_calls: program.managed_calls,
            child: child.as_ref().map(|c| c.id.clone()),
            child_status: child.as_ref().map(|c| c.habitat_status().into()),
            receipt: parent.program_receipt.clone(),
        }))
    }

    /// Export only a completed program's original results for offline replay.
    /// Reading records grants no authority to dispatch or repeat its children.
    pub fn program_record(&self, id: &Id) -> Result<Value> {
        let db = self.db()?;
        let task = task_from(&db, id)?.ok_or(Error::Unavailable("program task not found"))?;
        let program = task
            .program
            .as_ref()
            .ok_or(Error::Unavailable("task is not a program"))?;
        if task.state != TaskState::Completed || task.program_waiting {
            return Err(Error::Unavailable(
                "program must finish before exporting its results",
            ));
        }
        let execution =
            read_execution(&db, &task.id)?.ok_or(Error::Conflict("program record missing"))?;
        if task.program_receipt.as_ref() != Some(&execution.receipt)
            || execution.checkpoint["outcome"] != "complete"
        {
            return Err(Error::Conflict("completed program record changed"));
        }
        let mut results = Vec::new();
        let mut children = Vec::new();
        for index in 1..=execution.calls {
            let call = read_call(&db, &task.id, index)?;
            let settled = call
                .result
                .ok_or(Error::Conflict("program result missing"))?;
            results.push(ProgramCallResult {
                request_digest: call.request.digest,
                summary: settled.summary,
            });
            children.push(call.child);
        }
        Ok(
            json!({"program":program,"results":results,"children":children,
            "receiptDigest":execution.receipt}),
        )
    }
    pub(super) fn program_dependency_sessions(&self) -> Result<BTreeSet<Id>> {
        let db = self.db()?;
        let mut query = db.prepare("SELECT DISTINCT c.child FROM program_calls c JOIN tasks parent ON parent.id=c.parent WHERE parent.state NOT IN ('completed','failed','cancelled') LIMIT 1025")?;
        let children = query
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if children.len() > 1024 {
            return Err(xcb_core::Error::Limit("program session dependencies").into());
        }
        let mut sessions = BTreeSet::new();
        for id in children {
            let child = task_from(&db, &Id::new(id)?)?
                .ok_or(Error::Conflict("program dependency child missing"))?;
            if let Some(session) = child.session {
                sessions.insert(session);
            }
            sessions.extend(child.worker_sessions);
        }
        Ok(sessions)
    }

    pub(super) fn program_slice_input(
        &self,
        task: &ManagedTask,
    ) -> Result<(Option<Value>, Option<ProgramCallResult>)> {
        let db = self.db()?;
        let execution = read_execution(&db, &task.id)?;
        match execution {
            None if task.program_receipt.is_none() && !task.program_waiting => Ok((None, None)),
            Some(execution)
                if execution.waiting.is_none()
                    && !task.program_waiting
                    && task.program_receipt.as_ref() == Some(&execution.receipt) =>
            {
                if execution.calls > 0 {
                    let call = read_call(&db, &task.id, execution.calls)?;
                    let result = call
                        .result
                        .ok_or(Error::Conflict("program response evidence is missing"))?;
                    let response = execution
                        .response
                        .as_ref()
                        .ok_or(Error::Conflict("program resume response is missing"))?;
                    if response.request_digest != call.request.digest
                        || response.summary != result.summary
                    {
                        return Err(Error::Conflict("program response evidence changed"));
                    }
                }
                Ok((Some(execution.checkpoint), execution.response))
            }
            _ => Err(Error::Conflict(
                "program checkpoint does not match task phase",
            )),
        }
    }
    async fn program_child(
        &self,
        parent: &ManagedTask,
        call: &ProgramCall,
        index: u8,
        policy: ProjectPolicy,
    ) -> Result<ChildPublication> {
        if !valid_digest(&call.digest)
            || call.prompt.trim().is_empty()
            || call.prompt.len() > MAX_PROMPT_BYTES
        {
            return Err(Error::Conflict("invalid program worker request"));
        }
        let context = self.capture_program_context(parent, call, index)?;
        let source = Id::new(format!(
            "m_{}",
            digest(format!(
                "xcb-program-child-v1\0{}\0{index}\0{}",
                parent.id, call.digest
            ))
        ))?;
        let id = Id::new(format!(
            "t_{}",
            digest(format!(
                "xcb-task-v1\0{}\0{}\0{}",
                parent.conversation, source, parent.workspace
            ))
        ))?;
        let (preference, required) =
            self.initial_route_preferences(Path::new(&parent.workspace), &call.prompt)?;
        let routing_question = policy
            .required_provider
            .is_some_and(|p| required && preference != Some(p));
        let now = now_ms().max(parent.updated_at_ms);
        let mut task = ManagedTask {
            requirements: Default::default(),
            version: 1, id, operation: Id::new(format!("op_{}", digest(format!("xcb-program-operation-v1\0{source}"))))?,
            source_message: source.clone(), conversation: parent.conversation.clone(), workspace: parent.workspace.clone(),
            title: xcb_core::display_text(call.prompt.lines().find(|line| !line.trim().is_empty()).unwrap_or("Program worker"),160),
            goal: call.prompt.clone(), next_prompt: call.prompt.clone(), user_inputs: vec![], delivered_inputs: 0, context_carried: false,
            delivered_preferences: String::new(), input_at_ms: None, attachments: vec![], session: None, worker_sessions: vec![],
            route: policy.required_provider.or(preference).map(|p| p.to_string()), route_reason: None,
            provider_preference: policy.required_provider.or(preference), provider_required: policy.required_provider.is_some() || required, required_model: None,
            tried_routes: vec![], failed_accounts: vec![], retry: None, completion_review: None, state: if routing_question { TaskState::NeedsInput } else { TaskState::Queued },
            deferred: false, priority: parent.priority, attention: routing_question.then_some(State::NeedsAnswer), backlog_prompt: None,
            project_proposal: None, routing_question, program: None, program_generation: None, program_receipt: None, program_waiting: false,
            program_child: Some(ProgramChild { parent: parent.id.clone(), call: index, request_digest: call.digest.clone(), generation: policy.generation.clone(), required_provider: policy.required_provider, context: Some(context.reference.clone()) }), daemon_child: None,
            schedule: None, binding: habitat::inherited_binding(&parent.conversation, BindingOrigin::Program, format!("from {}", parent.id)), hold_until_ms: None, moved_from: None, detail: if routing_question { "This program request conflicts with the project provider requirement. Reply to this child with revised work for the required provider, or cancel it." } else { "managed program child; waiting for an eligible worker" }.into(),
            settle: None, acted: None, inbox_continuation: false, attempts: 0, max_attempts: MAX_TASK_ATTEMPTS, message_count_before: 0,
            cancel_requested: false, dismissed: false, last_output: None, policy_digest: parent.policy_digest.clone(), last_receipt: "sha256:pending".into(), revision: 1, created_at_ms: now, updated_at_ms: now,
        };
        let (_, receipt_digest, receipt) = Self::algal_receipt(&task).await?;
        task.last_receipt = receipt_digest;
        task.validate()?;
        bounded_text(
            &worker_prompt(&task, &[], &[], false),
            xcb_core::MAX_TEXT_BYTES,
        )?;
        let user = Message {
            id: source,
            role: Role::User,
            text: call.prompt.clone(),
            at_ms: now,
            attachments: vec![],
            provenance: None,
        };
        let ack = Self::assistant(
            format!(
                "**{}** requested child **{}** (call {index}).",
                parent.title, task.title
            ),
            Some(&task.id),
            task.revision,
        );
        Ok(ChildPublication {
            task,
            receipt,
            user,
            ack,
            policy,
            context,
        })
    }
    pub(super) async fn finish_program_slice(
        &self,
        id: &Id,
        revision: u64,
        result: &Result<ProgramSlice>,
    ) -> Result<ManagedTask> {
        let task = self
            .task(id)?
            .ok_or(Error::Unavailable("program task not found"))?;
        if task.state.terminal() || task.state != TaskState::Running {
            return Ok(task);
        }
        if task.cancel_requested {
            return self
                .finish_program(
                    id,
                    &Err(Error::Unavailable("program cancelled; output discarded")),
                )
                .await;
        }
        if task.revision != revision {
            return Err(Error::Conflict("program slice revision changed"));
        }
        let slice = match result {
            Ok(slice) => slice,
            Err(Error::Unavailable(
                "program cancelled before execution" | "program cancelled; output discarded",
            )) => {
                return self
                    .finish_program(
                        id,
                        &Err(Error::Unavailable("program cancelled; output discarded")),
                    )
                    .await;
            }
            Err(_) => {
                return self
                    .finish_program(id, &Err(Error::Unavailable("managed ALGAL slice failed")))
                    .await;
            }
        };
        if !valid_digest(&slice.receipt_digest)
            || serde_json::to_vec(&slice.checkpoint)?.len() > MAX_CHECKPOINT_BYTES
        {
            return Err(Error::Conflict("invalid program slice evidence"));
        }
        let previous = read_execution(&*self.db()?, id)?;
        if previous.as_ref().is_some_and(|e| e.waiting.is_some()) {
            return Err(Error::Conflict("program already waits for a child"));
        }
        let mut next = task.clone();
        next.revision += 1;
        next.updated_at_ms = now_ms().max(task.updated_at_ms);
        next.program_receipt = Some(slice.receipt_digest.clone());
        match &slice.outcome {
            ProgramSliceOutcome::Complete(report) => {
                if report.prompt.as_ref().is_some_and(|p| !p.trim().is_empty()) {
                    return self
                        .finish_program(
                            id,
                            &Err(Error::Unavailable(
                                "managed programs cannot emit untracked follow-up proposals",
                            )),
                        )
                        .await;
                }
                bounded_text(&report.summary, MAX_SUMMARY_BYTES)?;
                next.state = TaskState::Completed;
                next.program_waiting = false;
                next.last_output = Some(report.summary.clone());
                next.detail =
                    "managed ALGAL program completed; linked child reports retained".into();
                let execution = Execution {
                    parent: id.clone(),
                    checkpoint: slice.checkpoint.clone(),
                    receipt: slice.receipt_digest.clone(),
                    calls: previous.as_ref().map_or(0, |e| e.calls),
                    waiting: None,
                    response: None,
                };
                let message = Self::assistant(
                    format!("**{}** · {}", task.title, report.summary),
                    Some(id),
                    next.revision,
                );
                self.transition_program(
                    &task,
                    next,
                    Some(message),
                    &[],
                    None,
                    None,
                    None,
                    Some(&Change::Complete {
                        previous,
                        execution,
                    }),
                )
                .await
            }
            ProgramSliceOutcome::Awaiting(call) => {
                let index = previous.as_ref().map_or(1, |e| e.calls.saturating_add(1));
                if index > task.program.as_ref().map_or(0, |p| p.managed_calls) {
                    return self
                        .finish_program(
                            id,
                            &Err(Error::Unavailable("managed program call budget exhausted")),
                        )
                        .await;
                }
                let policy = {
                    let db = self.db()?;
                    require_grant(
                        &db,
                        &task.workspace,
                        task.program_generation.as_ref(),
                        now_ms(),
                        true,
                    )
                };
                let policy = match policy {
                    Ok(policy) => policy,
                    Err(Error::Conflict(reason)) => return self.hold_program(&task, reason).await,
                    Err(error) => return Err(error),
                };
                let child = self.program_child(&task, call, index, policy).await?;
                let record = Call {
                    parent: id.clone(),
                    index,
                    request: call.clone(),
                    child: child.task.id.clone(),
                    result: None,
                    context: Some(child.context.clone()),
                };
                next.state = TaskState::Queued;
                next.program_waiting = true;
                next.detail = format!(
                    "waiting for child {} (call {index}); no worker slot held",
                    child.task.id
                );
                let execution = Execution {
                    parent: id.clone(),
                    checkpoint: slice.checkpoint.clone(),
                    receipt: slice.receipt_digest.clone(),
                    calls: index,
                    waiting: Some(index),
                    response: None,
                };
                match self
                    .transition_program(
                        &task,
                        next,
                        None,
                        &[],
                        None,
                        None,
                        None,
                        Some(&Change::Publish {
                            previous,
                            execution,
                            call: record,
                            child: Box::new(child),
                        }),
                    )
                    .await
                {
                    Err(Error::Conflict(reason))
                        if reason.starts_with("project authority")
                            || reason == "program waits for other project work to settle"
                            || reason == "project hourly start limit reached" =>
                    {
                        self.hold_program(&task, reason).await
                    }
                    result => result,
                }
            }
        }
    }
    async fn hold_program(&self, task: &ManagedTask, reason: &str) -> Result<ManagedTask> {
        let mut next = task.clone();
        next.state = TaskState::Queued;
        next.detail = reason.into();
        next.revision += 1;
        next.updated_at_ms = now_ms().max(task.updated_at_ms);
        self.transition(task, next, None).await
    }
    /// Program inputs have a bounded resume envelope, while the complete
    /// provider report remains in the child session receipt. Compact only the
    /// VM handoff so an enthusiastic worker cannot strand its parent.
    fn resume_summary(summary: &str) -> String {
        let fits = summary.len() <= MAX_SUMMARY_BYTES
            && algal::canonical::canonical(&json!(summary))
                .is_ok_and(|encoded| encoded.len() <= 16_384);
        if fits {
            return summary.to_owned();
        }
        let marker = format!(
            "\n[child report compacted; full report digest sha256:{}]\n",
            digest(summary.as_bytes())
        );
        let budget = MAX_SUMMARY_BYTES.saturating_sub(marker.len());
        let head_budget = budget / 2;
        let tail_budget = budget.saturating_sub(head_budget);
        let head = xcb_core::display_text(summary, head_budget);
        let mut tail = String::new();
        for ch in summary.chars().rev() {
            if tail.len() + ch.len_utf8() > tail_budget {
                break;
            }
            tail.insert(0, ch);
        }
        format!("{head}{marker}{tail}")
    }

    pub(super) fn child_settlement(
        &self,
        store: &Store,
        child: &ManagedTask,
    ) -> Result<Option<Settlement>> {
        if !matches!(
            child.state,
            TaskState::Completed | TaskState::Failed | TaskState::Cancelled
        ) {
            return Ok(None);
        }
        if store.unsettled_runs()?.iter().any(|run| {
            run.session.as_ref().is_some_and(|id| {
                child.session.as_ref() == Some(id) || child.worker_sessions.contains(id)
            })
        }) {
            return Ok(None);
        }
        // An owner-dismissed child has no provable outcome; it settles as
        // failed from its own record once no run holds it.
        let outcome = if child.state == TaskState::Failed
            && (child.dismissed || child.detail == super::DISMISSED_DETAIL)
        {
            None
        } else if let Some(session) = &child.session {
            match store.settled_outcome(session, child.message_count_before)? {
                Some(outcome)
                    if !outcome.facts.joined || outcome.facts.effects == EffectState::Uncertain =>
                {
                    return Ok(None);
                }
                Some(outcome) if child.state != TaskState::Completed => {
                    // A failed or cancelled child cannot answer its pending
                    // question, so the question no longer holds the program.
                    Some(outcome)
                }
                Some(outcome)
                    if !outcome.facts.pending_attention
                        && settled_completion(&outcome)
                        && outcome.facts.failure.is_none() =>
                {
                    Some(outcome)
                }
                Some(_) => return Ok(None),
                // A child cancelled while queued or awaiting input settles
                // from its own record once no run holds it; its last turn
                // may be absent or superseded.
                None if child.state != TaskState::Completed => None,
                None => return Ok(None),
            }
        } else if child.attempts != 0 || child.state == TaskState::Completed {
            return Ok(None);
        } else {
            None
        };
        let retained: bool = self.db()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM receipts WHERE digest=?1 AND task=?2 AND revision=?3)",
            params![child.last_receipt, child.id.as_str(), sql(child.revision)?],
            |r| r.get(0),
        )?;
        if !retained {
            return Err(Error::Conflict("program child receipt missing"));
        }
        // Preserve the complete report in the child session receipt. The VM
        // receives only the bounded handoff above, with a digest marker, so a
        // verbose worker cannot strand its parent on a size limit.
        let full_summary = outcome.as_ref().map_or_else(
            || child.work_summary().to_owned(),
            |outcome| outcome.text.clone(),
        );
        let summary = Self::resume_summary(&full_summary);
        let outcome_digest = outcome
            .as_ref()
            .map(|outcome| serde_json::to_string(outcome).map(digest))
            .transpose()?;
        Ok(Some(Settlement {
            revision: child.revision,
            receipt: child.last_receipt.clone(),
            summary_digest: digest(&summary),
            summary,
            session: child.session.clone(),
            message_count_before: child.message_count_before,
            outcome_digest,
        }))
    }
    pub(super) async fn tick_programs(&self, store: &Store, advance: bool) -> Result<()> {
        let parents: Vec<_> = self
            .active_tasks(128)?
            .into_iter()
            .filter(|task| task.program_waiting)
            .collect();
        for parent in parents {
            if let Err(error) = self.tick_program(store, &parent, advance).await {
                if !matches!(error, Error::Conflict(_)) {
                    record_supervisor_fault(
                        self.root(),
                        &format!("program {} held: {}", parent.id, fault_text(&error)),
                    );
                }
                // Retain the pending checkpoint even if its evidence is damaged.
                // Recovery cannot turn absent evidence into another dispatch.
                let detail = match &error {
                    Error::Conflict(reason) if reason.starts_with("project authority") => {
                        (*reason).to_owned()
                    }
                    _ => format!(
                        "program waiting for linked child evidence: {}",
                        fault_text(&error)
                    ),
                };
                if parent.detail != detail {
                    let mut next = parent.clone();
                    next.detail = detail;
                    next.revision += 1;
                    next.updated_at_ms = now_ms().max(parent.updated_at_ms);
                    let _ = self.transition(&parent, next, None).await;
                }
            }
        }
        Ok(())
    }
    async fn tick_program(&self, store: &Store, parent: &ManagedTask, advance: bool) -> Result<()> {
        let execution = read_execution(&*self.db()?, &parent.id)?
            .ok_or(Error::Conflict("program checkpoint missing"))?;
        let index = execution
            .waiting
            .ok_or(Error::Conflict("program waiting call missing"))?;
        let mut call = read_call(&*self.db()?, &parent.id, index)?;
        let child = self
            .task(&call.child)?
            .ok_or(Error::Conflict("program child missing"))?;
        let link = child
            .program_child
            .as_ref()
            .ok_or(Error::Conflict("program child provenance missing"))?;
        if link.parent != parent.id
            || link.call != index
            || link.request_digest != call.request.digest
            || child.conversation != parent.conversation
            || child.workspace != parent.workspace
            || parent.program_receipt.as_ref() != Some(&execution.receipt)
        {
            return Err(Error::Conflict("program child provenance changed"));
        }
        if parent.cancel_requested {
            if !child.state.terminal() && !child.cancel_requested {
                let mut next = child.clone();
                next.cancel_requested = true;
                next.detail =
                    "parent program cancellation requested; waiting for confirmed settlement"
                        .into();
                next.revision += 1;
                next.updated_at_ms = now_ms().max(child.updated_at_ms);
                self.transition(&child, next, None).await?;
                return Ok(());
            }
            if self.child_settlement(store, &child)?.is_some() {
                self.verify_task(&child.id).await?;
                let mut next = parent.clone();
                next.state = TaskState::Cancelled;
                next.program_waiting = false;
                next.cancel_requested = false;
                next.detail = "program cancelled after linked child settled".into();
                next.revision += 1;
                next.updated_at_ms = now_ms().max(parent.updated_at_ms);
                let message = Self::assistant(
                    format!("**{}** · {}", parent.title, next.detail),
                    Some(&parent.id),
                    next.revision,
                );
                self.transition(parent, next, Some(message)).await?;
            }
            return Ok(());
        }
        if !advance {
            return Ok(());
        }
        require_grant(
            &*self.db()?,
            &parent.workspace,
            parent.program_generation.as_ref(),
            now_ms(),
            false,
        )?;
        let Some(result) = self.child_settlement(store, &child)? else {
            let detail = format!(
                "waiting for child {} · {}; no worker slot held",
                child.id,
                child.habitat_status()
            );
            if parent.detail != detail {
                let mut next = parent.clone();
                next.detail = detail;
                next.revision += 1;
                next.updated_at_ms = now_ms().max(parent.updated_at_ms);
                self.transition(parent, next, None).await?;
            }
            return Ok(());
        };
        self.verify_task(&child.id).await?;
        let report_fits = !result.summary.trim().is_empty()
            && result.summary.len() <= MAX_SUMMARY_BYTES
            && algal::canonical::canonical(&json!(result.summary))
                .is_ok_and(|encoded| encoded.len() <= 16384);
        if child.state != TaskState::Completed || !report_fits {
            let mut next = parent.clone();
            next.state = TaskState::Failed;
            next.program_waiting = false;
            next.detail = if child.state == TaskState::Completed {
                format!(
                    "program stopped because child {} report exceeds resume limits ({} bytes, 16384 encoded); complete report retained in child session",
                    child.id, MAX_SUMMARY_BYTES
                )
            } else {
                format!(
                    "program stopped because child {} settled as {}",
                    child.id,
                    child.state.as_str()
                )
            };
            next.last_output = report_fits.then_some(result.summary);
            next.revision += 1;
            next.updated_at_ms = now_ms().max(parent.updated_at_ms);
            let message = Self::assistant(
                format!("**{}** · {}", parent.title, next.detail),
                Some(&parent.id),
                next.revision,
            );
            self.transition(parent, next, Some(message)).await?;
            return Ok(());
        }
        let mut next_execution = execution.clone();
        next_execution.waiting = None;
        next_execution.response = Some(ProgramCallResult {
            request_digest: call.request.digest.clone(),
            summary: result.summary.clone(),
        });
        call.result = Some(result);
        let mut next = parent.clone();
        next.program_waiting = false;
        next.detail = format!("child {} settled; resuming pinned ALGAL program", child.id);
        next.revision += 1;
        next.updated_at_ms = now_ms().max(parent.updated_at_ms);
        self.transition_program(
            parent,
            next,
            None,
            &[],
            None,
            None,
            None,
            Some(&Change::Resume {
                previous: execution,
                execution: next_execution,
                call,
                child: Box::new(child),
            }),
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "managed_program_state_tests.rs"]
mod tests;
