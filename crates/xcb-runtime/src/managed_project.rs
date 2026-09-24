//! Explicit, bounded authority for project agents. A goal is guidance, never a
//! semantic proof that an arbitrary proposed task is in scope.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectPolicy {
    pub conversation: Id,
    pub generation: Id,
    pub goal: String,
    pub enabled: bool,
    pub max_tasks: u32,
    pub admitted_tasks: u32,
    pub expires_at_ms: u64,
    pub required_provider: Option<Provider>,
    pub revision: u64,
}
impl ProjectPolicy {
    fn validate(&self) -> Result<()> {
        habitat::validate_prompt(&self.goal)?;
        if self.max_tasks == 0
            || self.max_tasks > 100
            || self.admitted_tasks > self.max_tasks
            || self.revision == 0
        {
            return Err(xcb_core::Error::Invalid("project policy").into());
        }
        sql(self.expires_at_ms)?;
        Ok(())
    }
    pub fn status(&self) -> &'static str {
        if !self.enabled {
            "paused"
        } else if self.expires_at_ms <= now_ms() {
            "expired"
        } else if self.admitted_tasks >= self.max_tasks {
            "budget exhausted"
        } else {
            "following project"
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectProposal {
    pub parent: Id,
    pub generation: Id,
    pub admitted: bool,
    pub required_provider: Option<Provider>,
}

pub(super) fn migrate(db: &mut Connection) -> Result<()> {
    let version: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 3 {
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS project_policies(conversation TEXT PRIMARY KEY REFERENCES conversations(id),revision INTEGER NOT NULL,payload TEXT NOT NULL); CREATE TABLE IF NOT EXISTS project_memory(conversation TEXT PRIMARY KEY REFERENCES conversations(id),revision INTEGER NOT NULL,payload TEXT NOT NULL); PRAGMA user_version=3;")?;
        tx.commit()?;
    }
    Ok(())
}
pub(super) fn policy_from(db: &Connection, conversation: &Id) -> Result<Option<ProjectPolicy>> {
    let row: Option<(i64, String)> = db
        .query_row(
            "SELECT revision,substr(payload,1,65537) FROM project_policies WHERE conversation=?1",
            [conversation.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    row.map(|(revision, payload)| {
        bounded_text(&payload, 65536)?;
        let policy: ProjectPolicy = decode(&payload)?;
        policy.validate()?;
        if &policy.conversation != conversation || sql(policy.revision)? != revision {
            return Err(Error::Conflict("project policy index mismatch"));
        }
        Ok(policy)
    })
    .transpose()
}
pub(super) fn write_policy(tx: &Transaction<'_>, policy: &ProjectPolicy) -> Result<()> {
    policy.validate()?;
    tx.execute("INSERT INTO project_policies(conversation,revision,payload) VALUES(?1,?2,?3) ON CONFLICT(conversation) DO UPDATE SET revision=excluded.revision,payload=excluded.payload", params![policy.conversation.as_str(),sql(policy.revision)?,serde_json::to_string(policy)?])?;
    Ok(())
}
fn no_outstanding(db: &Connection, conversation: &Id, excluded: Option<&Id>) -> Result<bool> {
    let mut query = db.prepare("SELECT id,payload FROM tasks WHERE conversation=?1 AND state IN ('queued','running','needs_input','uncertain')")?;
    for row in query.query_map([conversation.as_str()], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        let (id, payload) = row?;
        if excluded.is_some_and(|except| except.as_str() == id) {
            continue;
        }
        let task: ManagedTask = decode(&payload)?;
        task.validate()?;
        if !task.deferred {
            return Ok(false);
        }
    }
    Ok(true)
}
pub(super) fn check_dispatch(db: &Connection, task: &ManagedTask, now: u64) -> Result<()> {
    program_state::check_dispatch(db, task, now)?;
    if task.schedule.is_some() && policy_from(db, &task.conversation)?.is_some_and(|p| !p.enabled) {
        return Err(Error::Conflict(
            "project authority is paused; scheduled work waits",
        ));
    }
    let Some(proposal) = task.project_proposal.as_ref().filter(|p| p.admitted) else {
        return Ok(());
    };
    let policy = policy_from(db, &task.conversation)?
        .ok_or(Error::Conflict("project authority is missing"))?;
    if !policy.enabled || policy.expires_at_ms <= now || policy.generation != proposal.generation {
        return Err(Error::Conflict(
            "project authority is paused, expired, or replaced",
        ));
    }
    Ok(())
}
pub(super) struct ProjectAdmission {
    policy: ProjectPolicy,
    now: u64,
}
impl ProjectAdmission {
    pub fn check_and_record(&self, tx: &Transaction<'_>, task: &ManagedTask) -> Result<()> {
        let policy = policy_from(tx, &task.conversation)?
            .ok_or(Error::Conflict("project policy changed"))?;
        let proposal = task
            .project_proposal
            .as_ref()
            .ok_or(Error::Conflict("task is not a project proposal"))?;
        if policy.revision != self.policy.revision
            || policy.generation != proposal.generation
            || !policy.enabled
            || policy.expires_at_ms <= self.now
            || policy.admitted_tasks >= policy.max_tasks
            || !task.deferred
            || task.state != TaskState::Queued
            || task.cancel_requested
        {
            return Err(Error::Conflict("project proposal no longer eligible"));
        }
        let parent = task_from(tx, &proposal.parent)?
            .ok_or(Error::Conflict("proposal parent is unavailable"))?;
        if parent.conversation != task.conversation
            || parent.state != TaskState::Completed
            || !no_outstanding(tx, &task.conversation, Some(&task.id))?
        {
            return Err(Error::Conflict("project waits for conclusive completion"));
        }
        if let Some(required) = policy.required_provider
            && route_hint(task.effective_prompt()).is_some_and(|hint| hint != required)
        {
            return Err(Error::Conflict(
                "proposal conflicts with project provider requirement",
            ));
        }
        let mut next = policy;
        next.admitted_tasks += 1;
        next.revision += 1;
        write_policy(tx, &next)
    }
}

impl ManagedStore {
    pub fn project_policy(&self, conversation: &Id) -> Result<Option<ProjectPolicy>> {
        let db = self.db()?;
        policy_from(&db, conversation)
    }
    pub fn configure_project_policy(
        &self,
        conversation: &Id,
        expected_revision: Option<u64>,
        goal: String,
        max_tasks: u32,
        expires_at_ms: u64,
        required_provider: Option<Provider>,
    ) -> Result<ProjectPolicy> {
        self.conversation(conversation)?
            .ok_or(Error::Unavailable("conversation not found"))?;
        let now = now_ms();
        if expires_at_ms < now.saturating_add(3_599_000)
            || expires_at_ms > now.saturating_add(30 * 24 * 60 * 60 * 1000)
        {
            return Err(xcb_core::Error::Invalid("project authority expiry").into());
        }
        let mut db = self.write_db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = policy_from(&tx, conversation)?;
        if current.as_ref().map(|p| p.revision) != expected_revision {
            return Err(Error::Conflict("project policy revision changed"));
        }
        let policy = ProjectPolicy {
            conversation: conversation.clone(),
            generation: new_id("grant"),
            goal,
            enabled: true,
            max_tasks,
            admitted_tasks: 0,
            expires_at_ms,
            required_provider,
            revision: expected_revision.unwrap_or(0) + 1,
        };
        write_policy(&tx, &policy)?;
        tx.commit()?;
        Ok(policy)
    }
    pub fn set_project_policy_enabled(
        &self,
        conversation: &Id,
        expected_revision: u64,
        enabled: bool,
    ) -> Result<ProjectPolicy> {
        let mut db = self.write_db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut policy = policy_from(&tx, conversation)?
            .ok_or(Error::Unavailable("project policy not found"))?;
        if policy.revision != expected_revision {
            return Err(Error::Conflict("project policy revision changed"));
        }
        policy.enabled = enabled;
        policy.revision += 1;
        write_policy(&tx, &policy)?;
        tx.commit()?;
        Ok(policy)
    }
    pub fn project_policies(&self) -> Result<Vec<ProjectPolicy>> {
        let db = self.db()?;
        let mut query = db.prepare(
            "SELECT conversation FROM project_policies ORDER BY conversation LIMIT 4096",
        )?;
        let ids = query
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut policies = Vec::new();
        for id in ids {
            match Id::new(id)
                .map_err(Error::from)
                .and_then(|id| policy_from(&db, &id))
            {
                Ok(Some(policy)) => policies.push(policy),
                _ => record_supervisor_fault(
                    self.root(),
                    "A project policy could not be decoded; its automatic work is paused and other projects continue",
                ),
            }
        }
        Ok(policies)
    }
    pub(super) fn project_rows(&self) -> Result<Vec<xcb_core::ui::ProjectRow>> {
        Ok(self
            .project_policies()?
            .into_iter()
            .map(|p| xcb_core::ui::ProjectRow {
                status: p.status().into(),
                conversation: p.conversation,
                goal: p.goal,
                enabled: p.enabled,
                remaining_tasks: p.max_tasks - p.admitted_tasks,
                expires_at_ms: p.expires_at_ms,
                required_provider: p.required_provider,
                revision: p.revision,
            })
            .collect())
    }
    pub(super) fn project_dispatch_block(
        &self,
        task: &ManagedTask,
    ) -> Result<Option<&'static str>> {
        let db = self.db()?;
        match check_dispatch(&db, task, now_ms()) {
            Ok(()) => Ok(None),
            Err(Error::Conflict(reason)) => Ok(Some(reason)),
            Err(error) => Err(error),
        }
    }
    pub(super) fn project_context(&self, conversation: &Id) -> Result<String> {
        let Some(policy) = self
            .project_policy(conversation)?
            .filter(|p| p.enabled && p.expires_at_ms > now_ms())
        else {
            return Ok(String::new());
        };
        let provider = policy
            .required_provider
            .map(|p| format!("Required provider: {p}. Proposals must preserve this requirement.\n"))
            .unwrap_or_default();
        Ok(format!(
            "\n\nProject goal (user-delegated guidance):\n{}\n{}You may propose concrete next work with xcb_backlog_add and close already-satisfied deferred work with xcb_backlog_complete, explaining the evidence. After conclusive completion the host can admit up to {} more proposals before this grant expires. Proposals must serve this goal; do not invent work to keep busy. User-added backlog still requires explicit release. Report a concise work summary.\n",
            policy.goal,
            provider,
            policy.max_tasks - policy.admitted_tasks
        ))
    }
    pub(super) async fn tick_projects(&self, now: u64) -> Result<()> {
        for policy in self
            .project_policies()?
            .into_iter()
            .filter(|p| p.enabled && p.expires_at_ms > now && p.admitted_tasks < p.max_tasks)
        {
            let mut candidates = self
                .backlog(Some(&policy.conversation), 256)?
                .into_iter()
                .filter(|t| {
                    t.conversation == policy.conversation
                        && t.deferred
                        && !t.cancel_requested
                        && t.project_proposal
                            .as_ref()
                            .is_some_and(|p| p.generation == policy.generation)
                })
                .collect::<Vec<_>>();
            candidates
                .sort_by_key(|t| (std::cmp::Reverse(t.priority), t.created_at_ms, t.id.clone()));
            for task in candidates {
                let mut next = task.clone();
                next.deferred = false;
                next.project_proposal
                    .as_mut()
                    .expect("filtered proposal")
                    .admitted = true;
                next.detail =
                    "admitted by bounded project authority; waiting for an eligible worker".into();
                next.revision += 1;
                next.updated_at_ms = now_ms().max(task.updated_at_ms);
                match self
                    .transition_project(
                        &task,
                        next,
                        None,
                        &[],
                        None,
                        Some(&ProjectAdmission {
                            policy: policy.clone(),
                            now,
                        }),
                    )
                    .await
                {
                    Ok(_) => break,
                    Err(Error::Conflict(
                        "proposal conflicts with project provider requirement",
                    )) => {
                        let mut question = task.clone();
                        question.deferred = false;
                        question.state = TaskState::NeedsInput;
                        question.routing_question = true;
                        question.attention = Some(State::NeedsAnswer);
                        question.detail="This proposal requests a different provider from the project's required provider. Reply with a revised task for the required provider, or cancel this proposal.".into();
                        question.revision += 1;
                        question.updated_at_ms = now_ms().max(task.updated_at_ms);
                        let message = Self::assistant(
                            question.detail.clone(),
                            Some(&task.id),
                            question.revision,
                        );
                        match self.transition(&task, question, Some(message)).await {
                            Ok(_) | Err(Error::Conflict(_)) => (),
                            Err(error) => record_supervisor_fault(self.root(), &fault_text(&error)),
                        };
                        break;
                    }
                    Err(Error::Conflict(_)) => continue,
                    Err(error) => {
                        record_supervisor_fault(
                            self.root(),
                            &format!("project admission failed: {}", fault_text(&error)),
                        );
                        break;
                    }
                }
            }
        }
        Ok(())
    }
    pub async fn complete_backlog(
        &self,
        id: &Id,
        expected_revision: u64,
        summary: String,
    ) -> Result<ManagedTask> {
        self.complete_backlog_inner(id, expected_revision, summary, None)
            .await
    }
    pub(super) async fn complete_backlog_inner(
        &self,
        id: &Id,
        expected_revision: u64,
        summary: String,
        mutation: Option<&habitat::WorkerMutation>,
    ) -> Result<ManagedTask> {
        habitat::validate_prompt(&summary)?;
        bounded_text(&summary, 8192)?;
        let task = self
            .task(id)?
            .ok_or(Error::Unavailable("backlog task not found"))?;
        if task.revision != expected_revision
            || !task.deferred
            || task.state != TaskState::Queued
            || task.session.is_some()
            || task.cancel_requested
            || mutation.is_some_and(|m| m.source.conversation != task.conversation)
        {
            return Err(Error::Conflict(
                "only current deferred work can be completed",
            ));
        }
        let mut next = task.clone();
        next.deferred = false;
        next.state = TaskState::Completed;
        next.last_output = Some(summary);
        next.detail = "backlog completed with reported evidence; no worker was dispatched".into();
        next.revision += 1;
        next.updated_at_ms = now_ms().max(task.updated_at_ms);
        let message = Self::assistant(
            format!("**{}** · {}", next.title, next.work_summary()),
            Some(id),
            next.revision,
        );
        self.transition_habitat(&task, next, Some(message), &[], mutation)
            .await
    }
    pub async fn reconcile_uncertain(
        &self,
        store: &Store,
        id: &Id,
        expected_revision: u64,
    ) -> Result<ManagedTask> {
        let task = self
            .task(id)?
            .ok_or(Error::Unavailable("managed task not found"))?;
        if task.state != TaskState::Uncertain || task.revision != expected_revision {
            return Err(Error::Conflict(
                "task is not the current uncertain revision",
            ));
        }
        if store.unsettled_runs()?.iter().any(|run| {
            run.session.as_ref().is_some_and(|s| {
                task.session.as_ref() == Some(s) || task.worker_sessions.contains(s)
            })
        }) {
            return Err(Error::Conflict(
                "worker process or effects still require recovery",
            ));
        }
        let session = task.session.as_ref().ok_or(Error::Conflict(
            "uncertain task has no exact worker evidence",
        ))?;
        let outcome = store
            .settled_outcome(session, task.message_count_before)?
            .ok_or(Error::Conflict(
                "exact terminal worker evidence is unavailable",
            ))?;
        if !outcome.facts.joined || outcome.facts.effects == EffectState::Uncertain {
            return Err(Error::Conflict("worker effects remain uncertain"));
        }
        let state = if settled_completion(&outcome) {
            TaskState::Completed
        } else {
            match outcome.state {
                State::NeedsAnswer | State::NeedsApproval | State::NeedsAction => {
                    TaskState::NeedsInput
                }
                State::Cancelled => TaskState::Cancelled,
                State::Failed => TaskState::Failed,
                _ => return Err(Error::Conflict("worker outcome is not conclusive")),
            }
        };
        let mut next = task.clone();
        next.state = state;
        next.attention = if state == TaskState::NeedsInput {
            Some(outcome.state)
        } else {
            None
        };
        next.last_output = Some(outcome.text);
        next.detail = "reconciled from exact settled worker evidence; no retry launched".into();
        next.revision += 1;
        next.updated_at_ms = now_ms().max(task.updated_at_ms);
        let batch = self.inbox_batch(&task.id)?;
        let unstarted = match &batch {
            Some(batch) => {
                store.settled_input_submission(&batch.session, batch.message_count)? == Some(false)
                    && outcome.facts.effects == EffectState::None
                    && !outcome.facts.pending_attention
            }
            None => false,
        };
        let delivered = match &batch {
            Some(batch) => {
                store.input_matches_digest(
                    &batch.session,
                    batch.message_count,
                    &batch.prompt_digest,
                )? && store.settled_input_submission(&batch.session, batch.message_count)?
                    == Some(true)
            }
            None => false,
        };
        if batch.is_some() && !delivered && !unstarted {
            return Err(Error::Conflict(
                "exact inbox prompt evidence is unavailable",
            ));
        }
        if unstarted {
            let batch = batch.as_ref().expect("unstarted requires a batch");
            if !batch.events.is_empty()
                && next.user_inputs.last() == Some(&inbox::render(&batch.events))
            {
                next.user_inputs.pop();
                next.delivered_inputs = next.delivered_inputs.min(next.user_inputs.len());
                next.delivered_preferences.clear();
            }
            next.state = if task.cancel_requested {
                TaskState::Cancelled
            } else {
                TaskState::NeedsInput
            };
            next.attention = (next.state == TaskState::NeedsInput).then_some(State::NeedsAction);
            next.detail = "reconciled exact evidence that the prompt was not submitted; guidance retained, no retry launched".into();
        }
        let message = Self::assistant(
            format!("**{}** · {}", next.title, next.detail),
            Some(id),
            next.revision,
        );
        self.transition_inbox(
            &task,
            next,
            Some(message),
            &[],
            None,
            None,
            Some(&inbox::Change::Finish {
                delivered,
                unstarted,
                stamp: None,
            }),
        )
        .await
    }
}

#[cfg(test)]
#[path = "managed_project_tests.rs"]
mod tests;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryBinding {
    pub conversation: Id,
    pub config: crate::wordcell::WordcellConfig,
    pub revision: u64,
}
impl ManagedStore {
    pub fn memory_binding(&self, conversation: &Id) -> Result<Option<MemoryBinding>> {
        let db = self.db()?;
        let row:Option<(i64,String)>=db.query_row("SELECT revision,payload FROM project_memory WHERE conversation=?1 AND length(payload)<=65536",[conversation.as_str()],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
        row.map(|(revision, payload)| {
            let binding: MemoryBinding = decode(&payload)?;
            if &binding.conversation != conversation
                || sql(binding.revision)? != revision
                || binding.revision == 0
            {
                return Err(Error::Conflict("memory binding index mismatch"));
            }
            Ok(binding)
        })
        .transpose()
    }
    pub fn bind_memory(
        &self,
        conversation: &Id,
        expected_revision: Option<u64>,
        config: crate::wordcell::WordcellConfig,
    ) -> Result<MemoryBinding> {
        config.verify()?;
        self.conversation(conversation)?
            .ok_or(Error::Unavailable("conversation not found"))?;
        let mut db = self.write_db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<i64> = tx
            .query_row(
                "SELECT revision FROM project_memory WHERE conversation=?1",
                [conversation.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if current != expected_revision.map(sql).transpose()? {
            return Err(Error::Conflict("memory binding revision changed"));
        }
        let binding = MemoryBinding {
            conversation: conversation.clone(),
            config,
            revision: u64::try_from(current.unwrap_or(0))
                .map_err(|_| Error::Conflict("memory revision invalid"))?
                + 1,
        };
        tx.execute("INSERT INTO project_memory(conversation,revision,payload) VALUES(?1,?2,?3) ON CONFLICT(conversation) DO UPDATE SET revision=excluded.revision,payload=excluded.payload",params![conversation.as_str(),sql(binding.revision)?,serde_json::to_string(&binding)?])?;
        tx.commit()?;
        Ok(binding)
    }
    pub async fn search_memory(
        &self,
        conversation: &Id,
        query: &str,
        limit: usize,
    ) -> Result<Value> {
        let binding = self
            .memory_binding(conversation)?
            .ok_or(Error::Unavailable(
                "project Wordcell memory is not configured",
            ))?;
        let (_cancel, cancelled) = watch::channel(false);
        binding.config.search(query, limit, cancelled).await
    }
    pub async fn promote_memory(
        &self,
        conversation: &Id,
        task_id: &Id,
        note: &str,
    ) -> Result<crate::wordcell::PromotionReceipt> {
        habitat::validate_prompt(note)?;
        let task = self
            .task(task_id)?
            .ok_or(Error::Unavailable("memory source task not found"))?;
        if &task.conversation != conversation {
            return Err(Error::Conflict("memory source belongs to another project"));
        }
        let binding = self
            .memory_binding(conversation)?
            .ok_or(Error::Unavailable(
                "project Wordcell memory is not configured",
            ))?;
        let promotion = crate::wordcell::Promotion {
            task_id: task.id.to_string(),
            conversation_id: conversation.to_string(),
            summary: note.to_owned(),
        };
        let custody = private::directory(&self.root.join("memory-promotions"))?;
        let (_cancel, cancelled) = watch::channel(false);
        binding
            .config
            .promote(&promotion, &custody, cancelled)
            .await
    }
    pub(super) async fn finish_program(
        &self,
        id: &Id,
        result: &Result<crate::managed_program::ProgramReport>,
    ) -> Result<ManagedTask> {
        let task = self
            .task(id)?
            .ok_or(Error::Unavailable("program task not found"))?;
        if task.state.terminal() {
            return Ok(task);
        }
        if task.program.is_none() || task.state != TaskState::Running || task.session.is_some() {
            return Err(Error::Conflict("program task changed"));
        }
        if !task.cancel_requested
            && let Ok(report) = result
            && let Some(prompt) = &report.prompt
        {
            let policy = self
                .project_policy(&task.conversation)?
                .filter(|policy| Some(&policy.generation) == task.program_generation.as_ref());
            let proposal = policy.map(|policy| ProjectProposal {
                parent: task.id.clone(),
                generation: policy.generation,
                admitted: false,
                required_provider: policy.required_provider,
            });
            self.create_habitat_task(
                &task.conversation,
                Id::new(format!(
                    "m_{}",
                    digest(format!("xcb-program-proposal-v1\0{}", task.id))
                ))?,
                prompt.clone(),
                vec![],
                Path::new(&task.workspace),
                habitat::CreateOptions {
                    deferred: true,
                    priority: 5,
                    program_parent: Some(&task),
                    proposal,
                    ..Default::default()
                },
            )
            .await?;
        }
        let mut next = task.clone();
        next.revision += 1;
        next.updated_at_ms = now_ms().max(task.updated_at_ms);
        if task.cancel_requested
            || matches!(
                result,
                Err(Error::Unavailable(
                    "program cancelled before execution" | "program cancelled; output discarded"
                ))
            )
        {
            next.state = TaskState::Cancelled;
            next.detail = if task.program.as_ref().is_some_and(|program| program.managed_calls > 0) {
                "managed ALGAL program cancelled after interpreter joined; completed child reports retained"
            } else {
                "ALGAL planner cancelled after bounded interpreter joined; no external effects"
            }.into();
        } else {
            match result {
                Ok(report) => {
                    next.state = TaskState::Completed;
                    next.last_output = Some(report.summary.clone());
                    next.program_receipt = Some(report.receipt_digest.clone());
                    next.detail =
                        "pinned ALGAL planner completed; optional next work saved in backlog"
                            .into();
                }
                Err(error) => {
                    next.state = TaskState::Failed;
                    next.detail = format!("ALGAL planner failed: {}", fault_text(error));
                }
            }
        }
        let message = Self::assistant(
            format!("**{}** · {}", next.title, next.work_summary()),
            Some(id),
            next.revision,
        );
        self.transition(&task, next, Some(message)).await
    }
}
