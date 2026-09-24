//! Durable ALGAL suspension. The interpreter never dispatches a provider: a
//! joined slice publishes an ordinary child and its checkpoint atomically.
use super::*;
use crate::managed_program::{
    AdmittedProgram, MAX_CHECKPOINT_BYTES, MAX_MANAGED_CALLS, MAX_PROMPT_BYTES, MAX_SUMMARY_BYTES,
    ProgramCall, ProgramCallResult, ProgramSlice, ProgramSliceOutcome,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramChild {
    pub parent: Id,
    pub call: u8,
    pub request_digest: String,
    pub generation: Id,
    pub required_provider: Option<Provider>,
}
impl ProgramChild {
    pub(super) fn validate(&self) -> Result<()> {
        if self.call == 0 || self.call > MAX_MANAGED_CALLS || !valid_digest(&self.request_digest) {
            return Err(Error::Conflict("invalid managed program child"));
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
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Settlement {
    revision: u64,
    receipt: String,
    summary: String,
    summary_digest: String,
    session: Option<Id>,
    message_count_before: usize,
    outcome_digest: Option<String>,
}
pub(super) struct ChildPublication {
    task: ManagedTask,
    receipt: String,
    user: Message,
    ack: Message,
    policy: ProjectPolicy,
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
        "SELECT child,digest,substr(payload,1,131073) FROM program_calls WHERE parent=?1 AND call_index=?2",
        params![parent.as_str(), index], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).optional()?.ok_or(Error::Conflict("program call evidence is missing"))?;
    if payload.len() > 131072 || digest(&payload) != hash {
        return Err(Error::Conflict("program call integrity failed"));
    }
    let call: Call = decode(&payload)?;
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
    bounded_text(&payload, 131072)?;
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
pub(super) fn require_grant(
    db: &Connection,
    conversation: &Id,
    generation: Option<&Id>,
    now: u64,
    budget: bool,
) -> Result<ProjectPolicy> {
    let policy = project::policy_from(db, conversation)?.ok_or(Error::Conflict(
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
        require_grant(tx, &task.conversation, Some(generation), now_ms(), true)?;
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
        require_grant(db, &task.conversation, Some(generation), now, false)?;
        if task.program_waiting {
            return Err(Error::Conflict("program waits for its linked child"));
        }
        if task.detail == "project authority task budget is exhausted" {
            require_grant(db, &task.conversation, Some(generation), now, true)?;
        }
        if task.detail == "program waits for other project work to settle" {
            no_other_work(db, task)?;
        }
    }
    if let Some(link) = &task.program_child {
        let policy = require_grant(db, &task.conversation, Some(&link.generation), now, false)?;
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
            || policy.required_provider != link.required_provider
            || (link.required_provider.is_some()
                && (!task.provider_required || task.provider_preference != link.required_provider))
        {
            return Err(Error::Conflict("program child authority changed"));
        }
    }
    Ok(())
}
fn no_other_work(db: &Connection, parent: &ManagedTask) -> Result<()> {
    let mut query = db.prepare("SELECT id,payload FROM tasks WHERE conversation=?1 AND id<>?2 AND state IN ('queued','running','needs_input','uncertain')")?;
    for row in query.query_map(
        params![parent.conversation.as_str(), parent.id.as_str()],
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
            {
                return Err(Error::Conflict("program call publication changed"));
            }
            let policy = require_grant(
                tx,
                &expected.conversation,
                expected.program_generation.as_ref(),
                now_ms(),
                true,
            )?;
            if !same(&policy, &child.policy)? {
                return Err(Error::Conflict(
                    "project authority changed before program publication",
                ));
            }
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
                &expected.conversation,
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

impl ManagedStore {
    pub async fn enqueue_program(
        &self,
        conversation: &Id,
        operation: Id,
        title: String,
        program: AdmittedProgram,
    ) -> Result<ManagedTask> {
        habitat::validate_prompt(&title)?;
        program.verify()?;
        let chat = self
            .conversation(conversation)?
            .ok_or(Error::Unavailable("conversation not found"))?;
        self.create_habitat_task(
            conversation,
            operation,
            title,
            vec![],
            Path::new(&chat.workspace),
            habitat::CreateOptions {
                program: Some(&program),
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
            version: 1, id, operation: Id::new(format!("op_{}", digest(format!("xcb-program-operation-v1\0{source}"))))?,
            source_message: source.clone(), conversation: parent.conversation.clone(), workspace: parent.workspace.clone(),
            title: xcb_core::display_text(call.prompt.lines().find(|line| !line.trim().is_empty()).unwrap_or("Program worker"),160),
            goal: call.prompt.clone(), next_prompt: call.prompt.clone(), user_inputs: vec![], delivered_inputs: 0, context_carried: false,
            delivered_preferences: String::new(), input_at_ms: None, attachments: vec![], session: None, worker_sessions: vec![],
            route: policy.required_provider.or(preference).map(|p| p.to_string()), route_reason: None,
            provider_preference: policy.required_provider.or(preference), provider_required: policy.required_provider.is_some() || required,
            tried_routes: vec![], failed_accounts: vec![], state: if routing_question { TaskState::NeedsInput } else { TaskState::Queued },
            deferred: false, priority: parent.priority, attention: routing_question.then_some(State::NeedsAnswer), backlog_prompt: None,
            project_proposal: None, routing_question, program: None, program_generation: None, program_receipt: None, program_waiting: false,
            program_child: Some(ProgramChild { parent: parent.id.clone(), call: index, request_digest: call.digest.clone(), generation: policy.generation.clone(), required_provider: policy.required_provider }),
            schedule: None, detail: if routing_question { "This program request conflicts with the project provider requirement. Reply to this child with revised work for the required provider, or cancel it." } else { "managed program child; waiting for an eligible worker" }.into(),
            settle: None, acted: None, inbox_continuation: false, attempts: 0, max_attempts: MAX_TASK_ATTEMPTS, message_count_before: 0,
            cancel_requested: false, last_output: None, policy_digest: parent.policy_digest.clone(), last_receipt: "sha256:pending".into(), revision: 1, created_at_ms: now, updated_at_ms: now,
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
                        &task.conversation,
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
                            || reason == "program waits for other project work to settle" =>
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
    fn child_settlement(&self, store: &Store, child: &ManagedTask) -> Result<Option<Settlement>> {
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
        let outcome = if let Some(session) = &child.session {
            let Some(outcome) = store.settled_outcome(session, child.message_count_before)? else {
                return Ok(None);
            };
            if !outcome.facts.joined
                || outcome.facts.effects == EffectState::Uncertain
                || outcome.facts.pending_attention
                || (child.state == TaskState::Completed
                    && (!settled_completion(&outcome) || outcome.facts.failure.is_some()))
            {
                return Ok(None);
            }
            Some(outcome)
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
        // Preserve the complete report. Provenance and status live alongside
        // it in the indexed call receipt; never acknowledge clipped content.
        // ManagedTask.last_output is a bounded display projection. The exact
        // terminal outcome is the source for VM resumption and size admission.
        let summary = outcome.as_ref().map_or_else(
            || child.work_summary().to_owned(),
            |outcome| outcome.text.clone(),
        );
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
            &parent.conversation,
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
            next.detail = if !report_fits {
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
