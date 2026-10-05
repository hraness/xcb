//! Explicit, bounded authority for project agents. A goal is guidance, never a
//! semantic proof that an arbitrary proposed task is in scope. A project is
//! a canonical workspace directory: every grant, admission and Wordcell
//! binding is keyed on `task.workspace`, never on a conversation.
use super::*;
use crate::workspace_infer::BindingOrigin;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectPolicy {
    /// The canonical project directory the grant covers.
    pub workspace: String,
    pub generation: Id,
    pub goal: String,
    pub enabled: bool,
    pub max_tasks: u32,
    pub admitted_tasks: u32,
    pub expires_at_ms: u64,
    pub required_provider: Option<Provider>,
    /// The repository's canonical common git directory, captured when the
    /// grant was configured. Linked worktrees share it, so automatic work in
    /// any of this repository's checkouts counts toward the herd's dials.
    /// `None` confines the herd to the exact workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// Concurrent provider lanes automatic work may hold across the herd's
    /// repository family; 0 defers breadth to the global adaptive target,
    /// account capacity and workspace serialization. Range: 0 to 64.
    #[serde(default)]
    pub max_active: u32,
    /// Automatic admissions this herd may start per hour — schedule
    /// occurrences, admitted proposals and managed children across the
    /// family. 0 is uncapped. Range: 0 to 512.
    #[serde(default)]
    pub max_per_hour: u32,
    pub revision: u64,
}
impl ProjectPolicy {
    pub(super) fn validate(&self) -> Result<()> {
        habitat::validate_prompt(&self.goal)?;
        bounded_text(&self.workspace, 4096)?;
        if let Some(repo) = &self.repo {
            bounded_text(repo, 4096)?;
            if !Path::new(repo).is_absolute() {
                return Err(xcb_core::Error::Invalid("project policy").into());
            }
        }
        if !Path::new(&self.workspace).is_absolute()
            || self.max_tasks == 0
            || self.max_tasks > 100
            || self.admitted_tasks > self.max_tasks
            || self.max_active > 64
            || self.max_per_hour > 512
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
    /// When the grant admitted this proposal; the herd's rate dial counts
    /// admissions by this instant, not the proposal's creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admitted_at_ms: Option<u64>,
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
pub(super) fn policy_from(db: &Connection, workspace: &str) -> Result<Option<ProjectPolicy>> {
    let row: Option<(i64, String)> = db
        .query_row(
            "SELECT revision,substr(payload,1,65537) FROM project_policies WHERE workspace=?1",
            [workspace],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    row.map(|(revision, payload)| {
        bounded_text(&payload, 65536)?;
        let policy: ProjectPolicy = decode(&payload)?;
        policy.validate()?;
        if policy.workspace != workspace || sql(policy.revision)? != revision {
            return Err(Error::Conflict("project policy index mismatch"));
        }
        Ok(policy)
    })
    .transpose()
}
pub(super) fn write_policy(tx: &Transaction<'_>, policy: &ProjectPolicy) -> Result<()> {
    policy.validate()?;
    tx.execute("INSERT INTO project_policies(workspace,revision,payload) VALUES(?1,?2,?3) ON CONFLICT(workspace) DO UPDATE SET revision=excluded.revision,payload=excluded.payload", params![policy.workspace,sql(policy.revision)?,serde_json::to_string(policy)?])?;
    Ok(())
}
/// Nonterminal tasks bound to `workspace`, other than `exclude`.
pub(super) fn outstanding_in(
    db: &Connection,
    workspace: &str,
    exclude: Option<&Id>,
) -> Result<Vec<ManagedTask>> {
    let mut query = db.prepare("SELECT id,payload FROM tasks WHERE workspace=?1 AND state IN ('queued','running','needs_input','uncertain') ORDER BY updated_at,id")?;
    let mut tasks = Vec::new();
    for row in query.query_map([workspace], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        let (id, payload) = row?;
        if exclude.is_some_and(|except| except.as_str() == id) {
            continue;
        }
        let task: ManagedTask = decode(&payload)?;
        task.validate()?;
        tasks.push(task);
    }
    Ok(tasks)
}
/// No undeferred nonterminal work is bound to `workspace`, other than `excluded`.
fn no_outstanding_in(db: &Connection, workspace: &str, excluded: Option<&Id>) -> Result<bool> {
    Ok(outstanding_in(db, workspace, excluded)?
        .iter()
        .all(|task| task.deferred))
}

/// The trailing window the herd's `max_per_hour` admission dial covers.
pub(super) const ADMISSION_WINDOW_MS: u64 = 3_600_000;

/// Whether `workspace` belongs to `policy`'s herd: the grant's own
/// directory, or a linked worktree of the same repository (`repo` was
/// captured when the grant was configured).
pub(super) fn herd_covers(policy: &ProjectPolicy, workspace: &str) -> bool {
    workspace == policy.workspace
        || policy.repo.as_deref().is_some_and(|repo| {
            workspace::repo_common_dir(Path::new(workspace)).as_deref() == Some(repo)
        })
}

/// The herd covering `workspace`: its own grant, else the grant whose
/// repository identity matches this workspace's common git directory. A
/// task admitted into a linked worktree joins its parent herd.
pub(super) fn herd_for(db: &Connection, workspace: &str) -> Result<Option<ProjectPolicy>> {
    if let Some(policy) = policy_from(db, workspace)? {
        return Ok(Some(policy));
    }
    let Some(repo) = workspace::repo_common_dir(Path::new(workspace)) else {
        return Ok(None);
    };
    let mut query =
        db.prepare("SELECT workspace FROM project_policies ORDER BY workspace LIMIT 4096")?;
    let workspaces = query
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for candidate in workspaces {
        if policy_from(db, &candidate)?.is_some_and(|p| p.repo.as_deref() == Some(repo.as_str())) {
            return policy_from(db, &candidate);
        }
    }
    Ok(None)
}

/// Automatic work the herd dispatches on its own authority: schedule
/// occurrences, admitted proposals and managed program or daemon children.
/// Operator submissions stay outside the herd's dials. ALGAL planner tasks
/// are orchestration, not provider lanes — their children carry the load.
pub(super) fn herd_automatic(task: &ManagedTask) -> bool {
    task.program.is_none()
        && (task.schedule.is_some()
            || task.project_proposal.is_some()
            || task.program_child.is_some()
            || task.daemon_child.is_some())
}

/// Automatic admissions across the herd family since `since_ms`: schedule
/// occurrences and managed children by creation, proposals by the instant
/// the grant admitted them. Membership is the herd's repository family, so
/// bursts in linked worktree lanes count the same as work in the checkout.
pub(super) fn admissions_in(db: &Connection, policy: &ProjectPolicy, since_ms: u64) -> Result<u32> {
    let mut query = db.prepare(
        "SELECT workspace FROM tasks WHERE CASE WHEN json_valid(payload) THEN
            (json_extract(payload,'$.schedule') IS NOT NULL AND json_extract(payload,'$.created_at_ms')>?1)
            OR json_extract(payload,'$.project_proposal.admitted_at_ms')>?1
            OR (json_extract(payload,'$.program_child') IS NOT NULL AND json_extract(payload,'$.created_at_ms')>?1)
            OR (json_extract(payload,'$.daemon_child') IS NOT NULL AND json_extract(payload,'$.created_at_ms')>?1)
        ELSE 0 END LIMIT 4096",
    )?;
    let mut cache: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    let mut count = 0u32;
    for row in query.query_map([sql(since_ms)?], |row| row.get::<_, String>(0))? {
        let workspace = row?;
        if *cache
            .entry(workspace.clone())
            .or_insert_with(|| herd_covers(policy, &workspace))
        {
            count = count.saturating_add(1);
        }
    }
    Ok(count)
}

/// Refuse an automatic admission once the herd's hourly dial is spent. The
/// occurrence or proposal stays queued and retries on a later pass; nothing
/// is dropped or retried speculatively.
pub(super) fn check_admission_window(
    db: &Connection,
    policy: &ProjectPolicy,
    now: u64,
) -> Result<()> {
    if policy.max_per_hour > 0
        && admissions_in(db, policy, now.saturating_sub(ADMISSION_WINDOW_MS))?
            >= policy.max_per_hour
    {
        return Err(Error::Conflict("project hourly start limit reached"));
    }
    Ok(())
}
/// A herd's live posture — the read model behind `xcb projects status`.
#[derive(Debug, Clone, Serialize)]
pub struct HerdStatus {
    #[serde(flatten)]
    pub policy: ProjectPolicy,
    /// Provider-bound family tasks still in flight.
    pub lanes: Vec<ManagedTask>,
    /// Nonterminal family tasks, deferred ones included.
    pub open: usize,
    /// Uncertain family tasks holding custody until reconciled.
    pub uncertain: usize,
    /// Automatic admissions inside the trailing hourly window.
    pub admissions_last_hour: u32,
    /// Schedules whose directory the herd covers.
    pub schedules: Vec<HabitatSchedule>,
}

/// Deferred, unadmitted proposals of one grant generation in `workspace`,
/// the only candidates a grant may release.
fn proposals_in(db: &Connection, workspace: &str, generation: &Id) -> Result<Vec<Id>> {
    let mut query = db.prepare("SELECT id FROM tasks WHERE workspace=?1 AND state='queued' AND CASE WHEN json_valid(payload) THEN json_extract(payload,'$.deferred')=1 AND json_extract(payload,'$.project_proposal.generation')=?2 ELSE 0 END ORDER BY updated_at,id LIMIT 256")?;
    let ids = query
        .query_map(params![workspace, generation.as_str()], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ids.into_iter()
        .map(|id| Id::new(id).map_err(Error::from))
        .collect()
}
pub(super) fn check_dispatch(db: &Connection, task: &ManagedTask, now: u64) -> Result<()> {
    program_state::check_dispatch(db, task, now)?;
    // A herd pause covers the whole repository family, so a schedule in a
    // linked worktree lane waits too.
    if task.schedule.is_some() && herd_for(db, &task.workspace)?.is_some_and(|p| !p.enabled) {
        return Err(Error::Conflict(
            "project authority is paused; scheduled work waits",
        ));
    }
    let Some(proposal) = task.project_proposal.as_ref().filter(|p| p.admitted) else {
        return Ok(());
    };
    let policy =
        policy_from(db, &task.workspace)?.ok_or(Error::Conflict("project authority is missing"))?;
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
        let policy =
            policy_from(tx, &task.workspace)?.ok_or(Error::Conflict("project policy changed"))?;
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
            || parent.workspace != task.workspace
            || parent.state != TaskState::Completed
            || !no_outstanding_in(tx, &task.workspace, Some(&task.id))?
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
        check_admission_window(tx, &policy, self.now)?;
        let mut next = policy;
        next.admitted_tasks += 1;
        next.revision += 1;
        write_policy(tx, &next)
    }
}

impl ManagedStore {
    /// Shim: the grant for a project view's workspace.
    pub fn project_policy(&self, conversation: &Id) -> Result<Option<ProjectPolicy>> {
        workspace::policy_for_conversation(&*self.db()?, conversation)
    }
    pub fn project_policy_in(&self, workspace: &str) -> Result<Option<ProjectPolicy>> {
        let db = self.db()?;
        policy_from(&db, workspace)
    }
    /// Shim: configure the grant for a project view's workspace.
    pub fn configure_project_policy(
        &self,
        conversation: &Id,
        expected_revision: Option<u64>,
        goal: String,
        max_tasks: u32,
        expires_at_ms: u64,
        required_provider: Option<Provider>,
    ) -> Result<ProjectPolicy> {
        self.configure_project_policy_dialed(
            conversation,
            expected_revision,
            goal,
            max_tasks,
            expires_at_ms,
            required_provider,
            0,
            0,
        )
    }
    /// Shim: configure the grant and throughput dials for a project view's
    /// workspace.
    #[allow(clippy::too_many_arguments)]
    pub fn configure_project_policy_dialed(
        &self,
        conversation: &Id,
        expected_revision: Option<u64>,
        goal: String,
        max_tasks: u32,
        expires_at_ms: u64,
        required_provider: Option<Provider>,
        max_active: u32,
        max_per_hour: u32,
    ) -> Result<ProjectPolicy> {
        let workspace = self.conversation_workspace(conversation)?;
        self.configure_project_policy_in(
            Path::new(&workspace),
            expected_revision,
            goal,
            max_tasks,
            expires_at_ms,
            required_provider,
            max_active,
            max_per_hour,
        )
    }
    /// Grant bounded project authority over a validated directory. Admits
    /// the directory and resolves its open upgrade grant conflicts. The
    /// herd dials bound automatic work across every linked worktree of the
    /// workspace's repository: `max_active` concurrent provider lanes, and
    /// `max_per_hour` automatic admissions; zero leaves each unbounded.
    #[allow(clippy::too_many_arguments)]
    pub fn configure_project_policy_in(
        &self,
        workspace: &Path,
        expected_revision: Option<u64>,
        goal: String,
        max_tasks: u32,
        expires_at_ms: u64,
        required_provider: Option<Provider>,
        max_active: u32,
        max_per_hour: u32,
    ) -> Result<ProjectPolicy> {
        let workspace = self.validate_workspace(workspace)?;
        let now = now_ms();
        if expires_at_ms < now.saturating_add(3_599_000)
            || expires_at_ms > now.saturating_add(30 * 24 * 60 * 60 * 1000)
        {
            return Err(xcb_core::Error::Invalid("project authority expiry").into());
        }
        let known: Vec<String> = self
            .known_workspaces(4096)?
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        let repo = workspace::repo_common_dir(Path::new(&workspace));
        let mut db = self.write_db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = policy_from(&tx, &workspace)?;
        if current.as_ref().map(|p| p.revision) != expected_revision {
            return Err(Error::Conflict("project policy revision changed"));
        }
        let policy = ProjectPolicy {
            workspace: workspace.clone(),
            generation: new_id("grant"),
            goal,
            enabled: true,
            max_tasks,
            admitted_tasks: 0,
            expires_at_ms,
            required_provider,
            repo,
            max_active,
            max_per_hour,
            revision: expected_revision.unwrap_or(0) + 1,
        };
        write_policy(&tx, &policy)?;
        workspace::admit_tx(&tx, &workspace, "grant", None, &known, now)?;
        workspace::resolve_conflicts_tx(&tx, &workspace, "grant", now)?;
        tx.commit()?;
        Ok(policy)
    }
    /// Change only a herd's lane and rate dials; goal, budget, generation,
    /// provider and expiry are untouched. Revision-checked like every
    /// policy write. Also refreshes the family identity when the workspace
    /// gained a repository since the grant was configured.
    pub fn update_project_throughput_in(
        &self,
        workspace: &str,
        expected_revision: u64,
        max_active: u32,
        max_per_hour: u32,
    ) -> Result<ProjectPolicy> {
        let mut db = self.write_db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut policy =
            policy_from(&tx, workspace)?.ok_or(Error::Unavailable("project policy not found"))?;
        if policy.revision != expected_revision {
            return Err(Error::Conflict("project policy revision changed"));
        }
        policy.max_active = max_active;
        policy.max_per_hour = max_per_hour;
        policy.repo = workspace::repo_common_dir(Path::new(workspace));
        policy.revision += 1;
        write_policy(&tx, &policy)?;
        tx.commit()?;
        Ok(policy)
    }
    /// The herd covering `workspace`, for status and dispatch surfaces.
    pub fn herd_policy_in(&self, workspace: &str) -> Result<Option<ProjectPolicy>> {
        let db = self.db()?;
        herd_for(&db, workspace)
    }
    /// Automatic admissions a herd started in its trailing hourly window.
    pub fn herd_admissions_last_hour(&self, policy: &ProjectPolicy) -> Result<u32> {
        let db = self.db()?;
        admissions_in(&db, policy, now_ms().saturating_sub(ADMISSION_WINDOW_MS))
    }
    /// Provider-bound tasks in flight across the herd's family — every
    /// running lane, automatic and operator-started alike.
    pub fn herd_lanes_in(&self, policy: &ProjectPolicy) -> Result<Vec<ManagedTask>> {
        let db = self.db()?;
        self.herd_lanes_on(&db, policy)
    }
    fn herd_lanes_on(&self, db: &Connection, policy: &ProjectPolicy) -> Result<Vec<ManagedTask>> {
        let mut query = db.prepare(
            "SELECT id FROM tasks WHERE state='running' ORDER BY updated_at,id LIMIT 256",
        )?;
        let ids = query
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut lanes = Vec::new();
        let mut cache: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
        for id in ids {
            let Some(task) = self.habitat_list_task(db, &id) else {
                continue;
            };
            if task.program.is_some() {
                continue;
            }
            if *cache
                .entry(task.workspace.clone())
                .or_insert_with(|| herd_covers(policy, &task.workspace))
            {
                lanes.push(task);
            }
        }
        Ok(lanes)
    }

    /// The herd's posture on an already-held read — `herd_status_in` holds
    /// the store lock, so its queries must take `db` instead of re-locking.
    fn herd_status_on(&self, db: &Connection, policy: ProjectPolicy) -> Result<HerdStatus> {
        let lanes = self.herd_lanes_on(db, &policy)?;
        let mut query = db.prepare(
            "SELECT id FROM tasks WHERE state IN ('queued','needs_input','uncertain') ORDER BY updated_at DESC,id LIMIT 512",
        )?;
        let ids = query
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut open = lanes.len();
        let mut uncertain = 0usize;
        let mut cache: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
        for id in ids {
            let Some(task) = self.habitat_list_task(db, &id) else {
                continue;
            };
            if !*cache
                .entry(task.workspace.clone())
                .or_insert_with(|| herd_covers(&policy, &task.workspace))
            {
                continue;
            }
            open += 1;
            if task.state == TaskState::Uncertain {
                uncertain += 1;
            }
        }
        let admissions_last_hour =
            admissions_in(db, &policy, now_ms().saturating_sub(ADMISSION_WINDOW_MS))?;
        let schedules = self
            .schedules_in(db, None)?
            .into_iter()
            .filter(|schedule| {
                habitat::schedule_workspace(db, schedule)
                    .ok()
                    .is_some_and(|workspace| herd_covers(&policy, &workspace))
            })
            .collect();
        Ok(HerdStatus {
            policy,
            lanes,
            open,
            uncertain,
            admissions_last_hour,
            schedules,
        })
    }
    /// A herd's live posture for `xcb projects status`: the grant, the
    /// provider lanes its family currently holds, open and uncertain work,
    /// this hour's admissions and the schedules feeding it.
    pub fn herd_status_in(&self, workspace: &str) -> Result<Option<HerdStatus>> {
        let db = self.db()?;
        herd_for(&db, workspace)?
            .map(|policy| self.herd_status_on(&db, policy))
            .transpose()
    }
    /// Shim: pause or resume the grant for a project view's workspace.
    pub fn set_project_policy_enabled(
        &self,
        conversation: &Id,
        expected_revision: u64,
        enabled: bool,
    ) -> Result<ProjectPolicy> {
        self.set_project_policy_enabled_in(
            &self.conversation_workspace(conversation)?,
            expected_revision,
            enabled,
        )
    }
    /// Resuming a grant resolves its workspace's open upgrade grant conflicts.
    pub fn set_project_policy_enabled_in(
        &self,
        workspace: &str,
        expected_revision: u64,
        enabled: bool,
    ) -> Result<ProjectPolicy> {
        let mut db = self.write_db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut policy =
            policy_from(&tx, workspace)?.ok_or(Error::Unavailable("project policy not found"))?;
        if policy.revision != expected_revision {
            return Err(Error::Conflict("project policy revision changed"));
        }
        policy.enabled = enabled;
        policy.revision += 1;
        write_policy(&tx, &policy)?;
        if enabled {
            workspace::resolve_conflicts_tx(&tx, workspace, "grant", now_ms())?;
        }
        tx.commit()?;
        Ok(policy)
    }
    /// Every readable grant, ordered by workspace.
    pub fn project_policies(&self) -> Result<Vec<ProjectPolicy>> {
        let db = self.db()?;
        let mut query =
            db.prepare("SELECT workspace FROM project_policies ORDER BY workspace LIMIT 4096")?;
        let workspaces = query
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut policies = Vec::new();
        for workspace in workspaces {
            match policy_from(&db, &workspace) {
                Ok(Some(policy)) => policies.push(policy),
                _ => record_supervisor_fault(
                    self.root(),
                    "A project policy could not be decoded; its automatic work is paused and other projects continue",
                ),
            }
        }
        Ok(policies)
    }
    /// A grant's status for `xcb projects` and the TUI: `active`, `paused`,
    /// `paused by upgrade` (an open upgrade conflict paused it), `expired` or
    /// `spent`.
    pub fn project_status(&self, policy: &ProjectPolicy) -> Result<&'static str> {
        Ok(if !policy.enabled {
            if workspace::paused_by_upgrade(&*self.db()?, &policy.workspace)? {
                "paused by upgrade"
            } else {
                "paused"
            }
        } else if policy.expires_at_ms <= now_ms() {
            "expired"
        } else if policy.admitted_tasks >= policy.max_tasks {
            "spent"
        } else {
            "active"
        })
    }
    pub(super) fn project_rows(&self) -> Result<Vec<xcb_core::ui::ProjectRow>> {
        let mut rows = Vec::new();
        for p in self.project_policies()? {
            let status = self.project_status(&p)?;
            rows.push(xcb_core::ui::ProjectRow {
                name: self.workspace_name(&p.workspace)?,
                status: status.into(),
                workspace: p.workspace,
                goal: p.goal,
                enabled: p.enabled,
                remaining_tasks: p.max_tasks - p.admitted_tasks,
                expires_at_ms: p.expires_at_ms,
                required_provider: p.required_provider,
                revision: p.revision,
            });
        }
        Ok(rows)
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
    pub(super) fn project_context_in(&self, workspace: &str) -> Result<String> {
        let Some(policy) = self
            .project_policy_in(workspace)?
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
            // A spent hourly dial leaves the herd's proposals deferred until
            // the window clears; the authority is never widened to compensate.
            if policy.max_per_hour > 0 {
                let spent = {
                    let db = self.db()?;
                    admissions_in(&db, &policy, now.saturating_sub(ADMISSION_WINDOW_MS))
                };
                match spent {
                    Ok(count) if count >= policy.max_per_hour => continue,
                    Ok(_) => (),
                    Err(error) => {
                        record_supervisor_fault(
                            self.root(),
                            &format!("herd admission window failed: {}", fault_text(&error)),
                        );
                        continue;
                    }
                }
            }
            let mut candidates = {
                let db = self.db()?;
                proposals_in(&db, &policy.workspace, &policy.generation)?
                    .iter()
                    .filter_map(|id| self.habitat_list_task(&db, id.as_str()))
                    .filter(|t| {
                        t.workspace == policy.workspace
                            && t.deferred
                            && !t.cancel_requested
                            && t.project_proposal
                                .as_ref()
                                .is_some_and(|p| p.generation == policy.generation)
                    })
                    .collect::<Vec<_>>()
            };
            candidates
                .sort_by_key(|t| (std::cmp::Reverse(t.priority), t.created_at_ms, t.id.clone()));
            for task in candidates {
                let mut next = task.clone();
                next.deferred = false;
                let proposal = next.project_proposal.as_mut().expect("filtered proposal");
                proposal.admitted = true;
                proposal.admitted_at_ms = Some(now);
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
            || mutation.is_some_and(|m| m.source.workspace != task.workspace)
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
    /// The canonical project directory the binding serves.
    pub workspace: String,
    pub config: crate::wordcell::WordcellConfig,
    pub revision: u64,
}
impl ManagedStore {
    /// Shim: the Wordcell binding for a project view's workspace.
    pub fn memory_binding(&self, conversation: &Id) -> Result<Option<MemoryBinding>> {
        self.memory_binding_in(&self.conversation_workspace(conversation)?)
    }
    pub fn memory_binding_in(&self, workspace: &str) -> Result<Option<MemoryBinding>> {
        let db = self.db()?;
        let row:Option<(i64,String)>=db.query_row("SELECT revision,payload FROM project_memory WHERE workspace=?1 AND length(payload)<=65536",[workspace],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
        row.map(|(revision, payload)| {
            let binding: MemoryBinding = decode(&payload)?;
            if binding.workspace != workspace
                || sql(binding.revision)? != revision
                || binding.revision == 0
            {
                return Err(Error::Conflict("memory binding index mismatch"));
            }
            Ok(binding)
        })
        .transpose()
    }
    /// Shim: bind Wordcell memory for a project view's workspace.
    pub fn bind_memory(
        &self,
        conversation: &Id,
        expected_revision: Option<u64>,
        config: crate::wordcell::WordcellConfig,
    ) -> Result<MemoryBinding> {
        let workspace = self.conversation_workspace(conversation)?;
        self.bind_memory_in(Path::new(&workspace), expected_revision, config)
    }
    /// Bind Wordcell memory to a validated directory. Admits the directory
    /// and resolves its open upgrade memory conflicts.
    pub fn bind_memory_in(
        &self,
        workspace: &Path,
        expected_revision: Option<u64>,
        config: crate::wordcell::WordcellConfig,
    ) -> Result<MemoryBinding> {
        config.verify()?;
        let workspace = self.validate_workspace(workspace)?;
        let known: Vec<String> = self
            .known_workspaces(4096)?
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        let now = now_ms();
        let mut db = self.write_db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<i64> = tx
            .query_row(
                "SELECT revision FROM project_memory WHERE workspace=?1",
                [&workspace],
                |row| row.get(0),
            )
            .optional()?;
        if current != expected_revision.map(sql).transpose()? {
            return Err(Error::Conflict("memory binding revision changed"));
        }
        let binding = MemoryBinding {
            workspace: workspace.clone(),
            config,
            revision: u64::try_from(current.unwrap_or(0))
                .map_err(|_| Error::Conflict("memory revision invalid"))?
                + 1,
        };
        tx.execute("INSERT INTO project_memory(workspace,revision,payload) VALUES(?1,?2,?3) ON CONFLICT(workspace) DO UPDATE SET revision=excluded.revision,payload=excluded.payload",params![workspace,sql(binding.revision)?,serde_json::to_string(&binding)?])?;
        workspace::admit_tx(&tx, &workspace, "memory", None, &known, now)?;
        workspace::resolve_conflicts_tx(&tx, &workspace, "memory", now)?;
        tx.commit()?;
        Ok(binding)
    }
    /// Shim: search the Wordcell memory of a project view's workspace.
    pub async fn search_memory(
        &self,
        conversation: &Id,
        query: &str,
        limit: usize,
    ) -> Result<Value> {
        self.search_memory_in(&self.conversation_workspace(conversation)?, query, limit)
            .await
    }
    pub async fn search_memory_in(
        &self,
        workspace: &str,
        query: &str,
        limit: usize,
    ) -> Result<Value> {
        let Some(binding) = self.memory_binding_in(workspace)? else {
            if workspace::open_conflict(&*self.db()?, workspace, "memory")? {
                return Err(Error::Unavailable(
                    "conflicting Wordcell bindings from upgrade; run xcb memory configure <dir>",
                ));
            }
            return Err(Error::Unavailable(
                "project Wordcell memory is not configured",
            ));
        };
        let (_cancel, cancelled) = watch::channel(false);
        binding.config.search(query, limit, cancelled).await
    }
    /// Promote a note into the Wordcell binding of the task's workspace. The
    /// promotion keeps the task's conversation as provenance, which is hashed
    /// into its request digest.
    pub async fn promote_memory(
        &self,
        task_id: &Id,
        note: &str,
    ) -> Result<crate::wordcell::PromotionReceipt> {
        habitat::validate_prompt(note)?;
        let task = self
            .task(task_id)?
            .ok_or(Error::Unavailable("memory source task not found"))?;
        let binding = self
            .memory_binding_in(&task.workspace)?
            .ok_or(Error::Unavailable(
                "project Wordcell memory is not configured",
            ))?;
        let promotion = crate::wordcell::Promotion {
            task_id: task.id.to_string(),
            conversation_id: task.conversation.to_string(),
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
                .project_policy_in(&task.workspace)?
                .filter(|policy| Some(&policy.generation) == task.program_generation.as_ref());
            let proposal = policy.map(|policy| ProjectProposal {
                parent: task.id.clone(),
                generation: policy.generation,
                admitted: false,
                required_provider: policy.required_provider,
                admitted_at_ms: None,
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
                    binding: habitat::inherited_binding(
                        &task.conversation,
                        BindingOrigin::Program,
                        format!("from {}", task.id),
                    ),
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
