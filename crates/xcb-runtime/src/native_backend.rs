use crate::{
    Error, Result,
    config::Config,
    private,
    process::Pin,
    store::{RunRecord, StatusFilter, StatusPage, StatusSection as StoreStatusSection, Store},
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use xcb_core::{
    Provider,
    session::{State, TaskRequirements},
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NativeConfig {
    pub scopes: Vec<NativeScope>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeScope {
    pub workspace: PathBuf,
    pub providers: Vec<Provider>,
    #[serde(default)]
    pub github_credentials: bool,
    #[serde(default)]
    pub read_only_roots: Vec<PathBuf>,
    #[serde(default)]
    pub git_metadata: Vec<PathBuf>,
}

impl NativeConfig {
    pub fn is_empty(&self) -> bool {
        self.scopes.is_empty()
    }

    pub fn validate(&self) -> Result<()> {
        if self.scopes.len() > 64 {
            return Err(Error::Unavailable("too many native workspace grants"));
        }
        let mut workspaces = std::collections::BTreeSet::new();
        for scope in &self.scopes {
            if scope.providers.is_empty()
                || scope.providers.len() > Provider::SUPPORTED.len()
                || scope
                    .providers
                    .iter()
                    .any(|provider| !Provider::SUPPORTED.contains(provider))
                || scope
                    .providers
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != scope.providers.len()
                || scope.read_only_roots.len() > 16
                || scope.git_metadata.len() > 4
                || !workspaces.insert(&scope.workspace)
            {
                return Err(Error::Unavailable("invalid native workspace grant"));
            }
            for path in std::iter::once(&scope.workspace)
                .chain(&scope.read_only_roots)
                .chain(&scope.git_metadata)
            {
                if !xcb_core::absolute_clean(path)
                    || !xcb_core::bounded_path(path.to_str().ok_or(Error::PrivateState)?)
                {
                    return Err(Error::Unavailable(
                        "native grant paths must be bounded absolute paths",
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn scope(&self, workspace: &Path, provider: Provider) -> Option<&NativeScope> {
        self.scopes
            .iter()
            .find(|scope| scope.workspace == workspace && scope.providers.contains(&provider))
    }
}

pub(crate) fn protected_paths(state: &Path) -> Result<Vec<PathBuf>> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or(Error::PrivateState)?);
    let home = xcb_core::canonical(home)?;
    let mut paths = vec![state.to_owned()];
    paths.extend(
        [
            ".ssh",
            ".aws",
            ".local/share/xcb",
            ".xcb",
            ".config/gh",
            ".git-credentials",
            ".netrc",
            ".npmrc",
            ".cargo/credentials.toml",
            ".codex",
            ".claude",
            ".config/devin",
            ".local/share/devin",
            "Library/Keychains",
        ]
        .map(|path| home.join(path)),
    );
    Ok(paths)
}

pub fn validate_grant(scope: &NativeScope, state: &Path) -> Result<()> {
    NativeConfig {
        scopes: vec![scope.clone()],
    }
    .validate()?;
    validate_scope(scope, state)
}

pub(crate) fn validate_scope(scope: &NativeScope, state: &Path) -> Result<()> {
    let home = xcb_core::canonical(PathBuf::from(
        std::env::var_os("HOME").ok_or(Error::PrivateState)?,
    ))?;
    let mut protected = protected_paths(state)?;
    protected.push(crate::command_tool::default_root()?);
    protected.push(crate::coordination::default_root()?);
    for path in std::iter::once(&scope.workspace)
        .chain(&scope.read_only_roots)
        .chain(&scope.git_metadata)
    {
        if xcb_core::canonical(path)? != *path
            || !path.is_dir()
            || path == &home
            || path.parent().is_none()
            || protected
                .iter()
                .any(|secret| path.starts_with(secret) || secret.starts_with(path))
        {
            return Err(Error::Unavailable(
                "native grant overlaps private state or has changed",
            ));
        }
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct QualificationPermit {
    version: u32,
    host_sha256: String,
    owner_instance: String,
    session: xcb_core::Id,
    scope: NativeScope,
    expires_at_ms: u64,
    request_sha256: String,
    consumed: Option<(xcb_core::Id, String)>,
}

fn request_digest(arguments: &serde_json::Value) -> Result<String> {
    let mut arguments = arguments.clone();
    arguments.sort_all_objects();
    Ok(crate::digest(serde_json::to_vec(&arguments)?))
}

pub(crate) fn qualification_permit(
    store: &Store,
    session: &xcb_core::session::Session,
    scope: NativeScope,
    arguments: &serde_json::Value,
) -> Result<PathBuf> {
    validate_grant(&scope, store.root())?;
    if scope.workspace != Path::new(&session.workspace)
        || scope.providers != vec![session.model.provider]
        || !scope.git_metadata.is_empty()
    {
        return Err(Error::Unavailable(
            "qualification permit is not confined to its fixture session",
        ));
    }
    private::directory(&store.root().join("qualification/native-sessions"))?;
    let path = store
        .root()
        .join("qualification/native-sessions")
        .join(format!("{}.json", session.id));
    let permit = QualificationPermit {
        version: 1,
        host_sha256: crate::process::host_identity()?.1,
        owner_instance: store.instance().into(),
        session: session.id.clone(),
        scope,
        expires_at_ms: crate::now_ms().saturating_add(300000),
        request_sha256: request_digest(arguments)?,
        consumed: None,
    };
    private::create(&path, &serde_json::to_vec(&permit)?)?;
    Ok(path)
}

pub(crate) fn admit(
    store: &Store,
    run: &RunRecord,
    workspace: &Path,
    arguments: &serde_json::Value,
    call: &str,
    consume: bool,
) -> Result<NativeScope> {
    store.verify_owned_run(run)?;
    let session = run
        .session
        .as_ref()
        .and_then(|id| store.session(id).ok().flatten())
        .ok_or(Error::Unavailable(
            "native commands require an owned task session",
        ))?;
    if session.workspace != workspace.to_string_lossy() {
        return Err(Error::Unavailable("native execution workspace changed"));
    }
    let scope = if session.requirements.native_execution {
        let config = Config::load(store.root())?.0;
        let scope = config
            .native_execution
            .scope(workspace, session.model.provider)
            .ok_or(Error::Unavailable(
                "native execution is not granted to this workspace and provider",
            ))?
            .clone();
        require_provider_qualification(
            store.root(),
            session.model.provider,
            scope.github_credentials,
        )?;
        scope
    } else {
        let path = store
            .root()
            .join("qualification/native-sessions")
            .join(format!("{}.json", session.id));
        let bytes = private::read(&path, 32768)
            .map_err(|_| Error::Unavailable("native execution was not granted to this task"))?;
        let mut permit: QualificationPermit = serde_json::from_slice(&bytes)?;
        let binding = (run.id.clone(), call.to_owned());
        if permit.version != 1
            || permit.host_sha256 != crate::process::host_identity()?.1
            || permit.owner_instance != store.instance()
            || permit.session != session.id
            || permit.scope.workspace != workspace
            || permit.scope.providers != vec![session.model.provider]
            || !permit.scope.git_metadata.is_empty()
            || permit.expires_at_ms <= crate::now_ms()
            || permit.expires_at_ms > crate::now_ms().saturating_add(300000)
            || permit.request_sha256 != request_digest(arguments)?
            || permit
                .consumed
                .as_ref()
                .is_some_and(|prior| prior != &binding)
        {
            return Err(Error::Unavailable(
                "native qualification permit does not authorize this exact command",
            ));
        }
        if consume && permit.consumed.is_none() {
            permit.consumed = Some(binding);
            private::replace(&path, &serde_json::to_vec(&permit)?, &crate::digest(bytes))?;
        }
        permit.scope
    };
    validate_scope(&scope, store.root())?;
    require_qualification(store.root())?;
    let pin = Pin::load(store.root(), session.model.provider)?;
    if !crate::runner::provider_admitted(store.root(), &pin) {
        return Err(Error::Unavailable("native provider is not admitted"));
    }
    Ok(scope)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderQualification {
    pub version: u32,
    pub host_sha256: String,
    pub provider_sha256: String,
    pub provider: Provider,
    pub session: xcb_core::Id,
    pub model: String,
    pub observed_at_ms: u64,
    pub github_credentials: bool,
    pub passed: bool,
}

pub fn require_provider_qualification(root: &Path, provider: Provider, github: bool) -> Result<()> {
    require_qualification(root)?;
    let receipt: ProviderQualification = serde_json::from_slice(&private::read(
        &root.join(format!("qualification/native-{provider}.json")),
        16384,
    )?)?;
    let pin = Pin::load(root, provider)?;
    if receipt.version != 1
        || receipt.provider != provider
        || !receipt.passed
        || receipt.host_sha256 != crate::process::host_identity()?.1
        || receipt.provider_sha256 != pin.sha256
        || github && !receipt.github_credentials
        || receipt.observed_at_ms > crate::now_ms().saturating_add(300000)
        || crate::now_ms().saturating_sub(receipt.observed_at_ms)
            > crate::qualification::MAX_RECEIPT_AGE_MS
    {
        return Err(Error::Unavailable(
            "native provider live qualification is absent, stale or lacks the credential check",
        ));
    }
    Ok(())
}

pub fn require_qualification(root: &Path) -> Result<()> {
    if !cfg!(target_os = "macos") {
        return Err(Error::Unavailable(NOT_QUALIFIED));
    }
    let receipt: Qualification = serde_json::from_slice(&private::read(
        &root.join("qualification/native-command.json"),
        16384,
    )?)?;
    if receipt.version != 1
        || receipt.policy_sha256 != crate::digest(include_bytes!("native-command.sb"))
        || receipt.host_sha256 != crate::process::host_identity()?.1
        || receipt.platform != std::env::consts::OS
        || !receipt.passed
        || receipt.observed_at_ms > crate::now_ms().saturating_add(300000)
        || crate::now_ms().saturating_sub(receipt.observed_at_ms)
            > crate::qualification::MAX_RECEIPT_AGE_MS
    {
        return Err(Error::Unavailable(
            "native command confinement qualification is absent or stale",
        ));
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Qualification {
    pub version: u32,
    pub policy_sha256: String,
    pub host_sha256: String,
    pub platform: String,
    pub observed_at_ms: u64,
    pub passed: bool,
}

pub const NOT_QUALIFIED: &str = "native execution requires a qualified supported host and an explicit workspace/provider grant; no broker or offline command fallback is permitted for this task";

pub const ACCEPTANCE_CASES: &[&str] = &[
    "native_tool_inventory",
    "workspace_read_write_confinement",
    "private_account_and_configuration_isolation",
    "native_shell_and_toolchain",
    "network_dns_and_https",
    "git_worktree_and_authorized_remote_effects",
    "provider_approval_readback",
    "denial_stops_continuation_and_failover",
    "cancellation_joins_descendants",
    "uncertain_effect_retains_custody",
    "restart_and_resume_preserve_execution_grant",
    "cross_provider_handoff_preserves_execution_grant",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeTransport {
    CodexAppServer,
    ClaudeStreamJson,
    DevinAcp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeApproval {
    CodexAutoReview,
    ClaudeAuto,
    DevinProviderReview,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeBackendStatus {
    pub provider: Provider,
    pub transport: NativeTransport,
    pub approval: NativeApproval,
    pub implemented: bool,
    pub qualified: bool,
    pub fallback_permitted: bool,
    pub required_cases: &'static [&'static str],
}

pub fn status(provider: Provider) -> NativeBackendStatus {
    let (transport, approval) = match provider {
        Provider::Codex => (
            NativeTransport::CodexAppServer,
            NativeApproval::CodexAutoReview,
        ),
        Provider::Claude => (
            NativeTransport::ClaudeStreamJson,
            NativeApproval::ClaudeAuto,
        ),
        Provider::Devin => (
            NativeTransport::DevinAcp,
            NativeApproval::DevinProviderReview,
        ),
    };
    NativeBackendStatus {
        provider,
        transport,
        approval,
        implemented: cfg!(target_os = "macos") && Provider::SUPPORTED.contains(&provider),
        qualified: false,
        fallback_permitted: false,
        required_cases: ACCEPTANCE_CASES,
    }
}

/// Exact selectors for the read-only local status projection. Free-text search
/// stays outside this contract; consumers can grep the JSONL rendering while
/// these filters remain the reliable API.
#[derive(Debug, Clone)]
pub struct StatusQuery {
    pub provider: Option<Provider>,
    pub account: Option<xcb_core::Id>,
    pub session: Option<xcb_core::Id>,
    pub state: Option<State>,
    /// Exact canonical workspace path for sessions, runs and effects, and
    /// for the managed schedules and projects sections.
    pub workspace: Option<String>,
    pub has_lease: bool,
    pub unsettled_effects: bool,
    pub pending_command_custody: bool,
    /// Applies independently to accounts, sessions, runs and tool effects.
    pub limit: u64,
    /// Stable offset cursor into each deterministic result order.
    pub cursor: u64,
}

impl Default for StatusQuery {
    fn default() -> Self {
        Self {
            provider: None,
            account: None,
            session: None,
            state: None,
            workspace: None,
            has_lease: false,
            unsettled_effects: false,
            pending_command_custody: false,
            limit: 64,
            cursor: 0,
        }
    }
}

fn status_section<T>(
    section: &StoreStatusSection<T>,
    render: impl Fn(&T) -> Result<serde_json::Value>,
) -> Result<serde_json::Value> {
    let mut records = Vec::with_capacity(section.records.len());
    for record in &section.records {
        records.push(render(record)?);
    }
    let returned = u64::try_from(records.len())
        .map_err(|_| xcb_core::Error::Invalid("status record count"))?;
    let next = section.offset.saturating_add(returned);
    let truncated = next < section.matched;
    Ok(serde_json::json!({
        "matched": section.matched,
        "cursor": section.offset,
        "returned": returned,
        "truncated": truncated,
        "nextCursor": truncated.then_some(next),
        "records": records,
    }))
}

/// Slice an in-memory list into the same page envelope the store sections
/// use; managed tables are small and fully materialized by design.
fn managed_page<T>(
    records: Vec<T>,
    cursor: u64,
    limit: u64,
    render: impl Fn(&T) -> serde_json::Value,
) -> Result<serde_json::Value> {
    let matched =
        u64::try_from(records.len()).map_err(|_| xcb_core::Error::Invalid("status count"))?;
    let start = usize::try_from(cursor)
        .unwrap_or(usize::MAX)
        .min(records.len());
    let end = start
        .saturating_add(usize::try_from(limit).unwrap_or(usize::MAX))
        .min(records.len());
    let rendered: Vec<_> = records[start..end].iter().map(&render).collect();
    let returned =
        u64::try_from(rendered.len()).map_err(|_| xcb_core::Error::Invalid("status count"))?;
    let next = cursor.saturating_add(returned);
    Ok(serde_json::json!({
        "matched": matched,
        "cursor": cursor,
        "returned": returned,
        "truncated": next < matched,
        "nextCursor": (next < matched).then_some(next),
        "records": rendered,
    }))
}

/// Schedule and project-herd records for the same read-only projection,
/// through the managed store's read-only handle. Prompt text and program
/// manifests stay out — this is an agent-facing inventory. Degrades to an
/// availability marker when managed state is absent or from another schema
/// version rather than failing the whole snapshot.
fn managed_status(root: &Path, query: &StatusQuery) -> serde_json::Value {
    let database = root.join("managed").join("managed.sqlite");
    if !database.is_file() {
        return serde_json::json!({"available": false, "diagnostic": "no managed state yet"});
    }
    let managed = match crate::managed::ManagedStore::open_read_only(root) {
        Ok(Some(store)) => store,
        Ok(None) => {
            return serde_json::json!({"available": false, "diagnostic": "no managed state yet"});
        }
        Err(error) => {
            return serde_json::json!({
                "available": false,
                "diagnostic": xcb_core::display_text(&error.to_string(), 256),
            });
        }
    };
    let now = crate::now_ms();
    let schedules = managed.schedule_views(None).and_then(|views| {
        let views: Vec<_> = views
            .into_iter()
            .filter(|view| {
                query
                    .workspace
                    .as_deref()
                    .is_none_or(|workspace| view.workspace.as_deref() == Some(workspace))
            })
            .collect();
        managed_page(views, query.cursor, query.limit, |view| {
            let schedule = &view.schedule;
            serde_json::json!({
                "id": &schedule.id,
                "conversation": &schedule.conversation,
                "workspace": &view.workspace,
                "kind": if schedule.program.is_some() { "program" } else { "prompt" },
                "enabled": schedule.enabled,
                "intervalMs": schedule.interval_ms,
                "nextDueMs": schedule.next_due_ms,
                "dueNow": schedule.enabled && schedule.next_due_ms <= now,
                "blocker": &view.blocker,
                "lastTask": &schedule.last_task,
                "lastTaskState": view.last_task_state.map(|state| state.as_str()),
                "lastTaskDetail": view.last_task_detail.as_deref().map(|detail| xcb_core::display_text(detail, 256)),
                "revision": schedule.revision,
                "createdAtMs": schedule.created_at_ms,
                "updatedAtMs": schedule.updated_at_ms,
            })
        })
    });
    let projects = managed.project_policies().and_then(|policies| {
        // A workspace filter also selects the herd that covers it, so a
        // linked worktree shows its checkout's project. Resolve once.
        let herd_workspace = query
            .workspace
            .as_deref()
            .map(|workspace| managed.herd_policy_in(workspace))
            .transpose()?
            .flatten()
            .map(|herd| herd.workspace);
        let policies: Vec<_> = policies
            .into_iter()
            .filter(|policy| {
                herd_workspace
                    .as_deref()
                    .is_none_or(|workspace| policy.workspace == workspace)
            })
            .collect();
        let mut herds = Vec::with_capacity(policies.len());
        for policy in policies {
            let herd = managed
                .herd_status_in(&policy.workspace)?
                .ok_or(xcb_core::Error::Invalid("project status"))?;
            herds.push((policy, herd));
        }
        managed_page(herds, query.cursor, query.limit, |(policy, herd)| {
            serde_json::json!({
                "workspace": &policy.workspace,
                "name": managed.workspace_name(&policy.workspace).ok(),
                "status": managed.project_status(policy).unwrap_or("unknown"),
                "enabled": policy.enabled,
                "repo": &policy.repo,
                "generation": &policy.generation,
                "maxActive": policy.max_active,
                "maxPerHour": policy.max_per_hour,
                "lanes": herd.lanes.len(),
                "openTasks": herd.open,
                "uncertainTasks": herd.uncertain,
                "admissionsLastHour": herd.admissions_last_hour,
                "maxTasks": policy.max_tasks,
                "admittedTasks": policy.admitted_tasks,
                "expiresAtMs": policy.expires_at_ms,
                "requiredProvider": policy.required_provider,
                "schedules": herd.schedules.len(),
                "revision": policy.revision,
            })
        })
    });
    match (schedules, projects) {
        (Ok(schedules), Ok(projects)) => serde_json::json!({
            "available": true,
            "schedules": schedules,
            "projects": projects,
        }),
        (Err(error), _) | (_, Err(error)) => serde_json::json!({
            "available": false,
            "diagnostic": xcb_core::display_text(&error.to_string(), 256),
        }),
    }
}

/// Local records for agent status checks. This projection never starts or
/// attaches to a provider, submits no prompt, and does not refresh quota or
/// credentials. It is a bounded snapshot, not a recovery decision.
pub fn status_snapshot(store: &Store, query: StatusQuery) -> Result<serde_json::Value> {
    if !(1..=256).contains(&query.limit) {
        return Err(xcb_core::Error::Invalid("status limit").into());
    }
    let now = crate::now_ms();
    let snapshot = store.native_status(
        &StatusFilter {
            provider: query.provider,
            account: query.account.clone(),
            session: query.session.clone(),
            state: query.state,
            workspace: query.workspace.clone(),
            has_lease: query.has_lease,
            unsettled_effects: query.unsettled_effects,
            pending_command_custody: query.pending_command_custody,
        },
        StatusPage {
            limit: query.limit,
            offset: query.cursor,
        },
        now,
    )?;
    let (config, _) = Config::load(store.root())?;
    let command_qualified = require_qualification(store.root()).is_ok();
    let backends: Vec<_> = Provider::SUPPORTED
        .into_iter()
        .map(|provider| {
            let mut status = status(provider);
            status.qualified = require_provider_qualification(store.root(), provider, false)
                .is_ok()
                && Pin::load(store.root(), provider)
                    .is_ok_and(|pin| crate::runner::provider_admitted(store.root(), &pin));
            status
        })
        .collect();
    let service = match xcb_core::home_dir() {
        Some(home) => match crate::habitat_service::status(store.root(), &home) {
            Ok(status) => serde_json::json!({
                "available": true,
                "installed": status.installed,
                "registered": status.registered,
                "supervisorRunning": status.supervisor_running,
                "supervisorHealth": {
                    "state": status.supervisor_health.state,
                    "heartbeatAgeSeconds": status.supervisor_health.heartbeat_age_seconds,
                },
                "watchdogEnabled": status.watchdog_enabled,
                "supervisorWatched": status.supervisor_watched,
                "label": status.service.as_ref().map(|service| &service.label),
            }),
            Err(error) => serde_json::json!({
                "available": false,
                "diagnostic": xcb_core::display_text(&error.to_string(), 256),
            }),
        },
        None => serde_json::json!({
            "available": false,
            "diagnostic": "home directory is unknown",
        }),
    };
    let mut coverage = serde_json::Map::new();
    for provider in Provider::SUPPORTED {
        coverage.insert(provider.as_str().to_owned(), method_coverage(provider)?);
    }

    let accounts = status_section(&snapshot.accounts, |record| {
        let account = &record.account;
        Ok(serde_json::json!({
            "id": &account.id,
            "provider": account.provider,
            "name": account.name(),
            "email": &account.email,
            "subscription": &account.subscription,
            "enabled": account.enabled,
            "authenticationRequired": record.authentication_required,
            "credentialsPresent": crate::auth::has_credentials(store, &account.id).unwrap_or(false),
            "busy": record.active_runs > 0,
            "activeRuns": record.active_runs,
            "leaseHeld": record.lease_held,
            "quota": {
                "remainingPercent": record.remaining_percent,
                "resetsAtMs": record.resets_at_ms,
                "blockedUntilMs": record.quota_blocked_until_ms,
            },
        }))
    })?;
    let sessions = status_section(&snapshot.sessions, |record| {
        let session = &record.session;
        Ok(serde_json::json!({
            "id": &session.id,
            "provider": session.model.provider,
            "account": &session.account,
            "model": &session.model,
            "workspace": &session.workspace,
            "state": session.state,
            "revision": session.revision,
            "routePins": &session.route_pins,
            "requirements": &session.requirements,
            "managedTask": &session.managed_task,
            "createdAtMs": session.created_at_ms,
            "lastActiveAtMs": session.last_active_at_ms,
            "runs": {
                "count": record.run_count,
                "unsettled": record.unsettled_run_count,
            },
            "effects": {
                "count": record.effect_count,
                "unsettled": record.unsettled_effect_count,
            },
            "leaseHeld": record.lease_held,
            "pendingCommandCustody": record.pending_command_custody,
            "capabilityProcessCount": record.capability_process_count,
        }))
    })?;
    let runs = status_section(&snapshot.runs, |record| {
        let run = &record.run;
        Ok(serde_json::json!({
            "id": &run.id,
            "session": &run.session,
            "account": &run.account,
            "provider": record.provider.or(run.model.as_ref().map(|model| model.provider)),
            "phase": &run.phase,
            "revision": run.revision,
            "createdAtMs": run.created_at_ms,
            "model": &run.model,
            "leaseHeld": record.lease_held,
            "owner": run.owner.as_ref().map(|owner| serde_json::json!({
                "instance": &owner.instance,
                "pid": owner.pid,
                "alive": crate::os::process_exists(owner.pid),
            })),
            "processGroup": run.pid,
            "commandCustody": &run.command_custody,
            "capabilityProcesses": &run.capability_processes,
            "effects": {
                "count": record.effect_count,
                "unsettled": record.unsettled_effect_count,
            },
        }))
    })?;
    let effects = status_section(&snapshot.effects, |record| {
        Ok(serde_json::json!({
            "session": &record.session,
            "account": &record.account,
            "provider": record.provider,
            "run": &record.run,
            "call": &record.call,
            "operation": &record.operation,
            "inputDigest": &record.input_digest,
            "settled": record.settled,
        }))
    })?;
    let mut managed = managed_status(store.root(), &query);
    let empty_section = || {
        serde_json::json!({
            "matched": 0,
            "cursor": query.cursor,
            "returned": 0,
            "truncated": false,
            "nextCursor": null,
            "records": [],
        })
    };
    let schedules = managed
        .as_object_mut()
        .and_then(|value| value.remove("schedules"))
        .unwrap_or_else(empty_section);
    let projects = managed
        .as_object_mut()
        .and_then(|value| value.remove("projects"))
        .unwrap_or_else(empty_section);

    Ok(serde_json::json!({
        "version": 1,
        "generatedAtMs": now,
        "inspection": {
            "localOnly": true,
            "providerAttached": false,
            "providerProcessesStarted": 0,
            "promptSubmitted": false,
            "resetCreditsConsumed": 0,
            "snapshot": "one deferred read transaction for local records",
        },
        "service": service,
        "native": {
            "commandQualified": command_qualified,
            "backends": backends,
            "workspaceGrantCount": config.native_execution.scopes.len(),
            "workspaceGrants": config.native_execution.scopes,
        },
        "methodCoverage": serde_json::Value::Object(coverage),
        "totals": {
            "accounts": snapshot.totals.accounts,
            "sessions": snapshot.totals.sessions,
            "runs": snapshot.totals.runs,
            "toolEffects": snapshot.totals.tool_effects,
            "leases": snapshot.totals.leases,
            "heldAccounts": snapshot.totals.held_accounts,
            "unsettledRuns": snapshot.totals.unsettled_runs,
            "unsettledEffects": snapshot.totals.unsettled_effects,
            "unlinkedToolEffects": snapshot.totals.unlinked_tool_effects,
            "pendingCommandCustody": snapshot.totals.pending_command_custody,
        },
        "filters": {
            "provider": query.provider,
            "account": query.account,
            "session": query.session,
            "state": query.state,
            "workspace": query.workspace,
            "hasLease": query.has_lease,
            "unsettledEffects": query.unsettled_effects,
            "pendingCommandCustody": query.pending_command_custody,
        },
        "pagination": {
            "limit": query.limit,
            "cursor": query.cursor,
            "cursorKind": "offset",
            "stableOrder": true,
        },
        "managed": managed,
        "schedules": schedules,
        "projects": projects,
        "accounts": accounts,
        "sessions": sessions,
        "runs": runs,
        "effects": effects,
    }))
}

pub fn inspect_session(store: &Store, id: &xcb_core::Id) -> Result<serde_json::Value> {
    let session = store
        .session(id)?
        .ok_or(Error::Unavailable("session not found"))?;
    let outcome = store.latest_settled_outcome(id)?;
    let receipts = store.session_receipts(id)?;
    let errors: Vec<_> = store
        .messages(id, 64)?
        .into_iter()
        .filter_map(|message| {
            if message.role != xcb_core::session::Role::Tool {
                return None;
            }
            let body = message.text.strip_prefix("workspace_native_exec: ")?;
            match serde_json::from_str::<serde_json::Value>(body) {
                Ok(value) => value.get("error").cloned(),
                Err(_) => Some(serde_json::Value::String(xcb_core::display_text(
                    body, 1024,
                ))),
            }
        })
        .collect();
    let runs: Vec<_> = receipts
        .runs
        .iter()
        .map(|receipt| {
            let run = &receipt.run;
            serde_json::json!({
                "id": &run.id,
                "phase": &run.phase,
                "account": &run.account,
                "revision": run.revision,
                "createdAtMs": run.created_at_ms,
                "model": &run.model,
                "leaseHeld": receipt.lease_held,
                "owner": run.owner.as_ref().map(|owner| serde_json::json!({
                    "instance": &owner.instance,
                    "pid": owner.pid,
                    "alive": crate::os::process_exists(owner.pid),
                })),
                "processGroup": run.pid,
                "commandCustody": &run.command_custody,
                "capabilityProcesses": &run.capability_processes,
                "toolEffects": receipts.effects.get(&run.id).map(|effects| effects.iter().map(|effect| {
                    serde_json::json!({
                        "call": &effect.call,
                        "operation": &effect.operation,
                        "inputDigest": &effect.input_digest,
                        "settled": effect.settled,
                    })
                }).collect::<Vec<_>>()).unwrap_or_default(),
            })
        })
        .collect();
    let shown_effects: u64 = receipts
        .runs
        .iter()
        .map(|receipt| {
            receipts
                .effects
                .get(&receipt.run.id)
                .map_or(0, |effects| effects.len() as u64)
        })
        .sum();
    Ok(serde_json::json!({
        "version": 1,
        "session": &session.id,
        "provider": session.model.provider,
        "account": &session.account,
        "model": &session.model,
        "workspace": &session.workspace,
        "state": session.state,
        "revision": session.revision,
        "routePins": &session.route_pins,
        "requirements": &session.requirements,
        "managedTask": &session.managed_task,
        "createdAtMs": session.created_at_ms,
        "lastActiveAtMs": session.last_active_at_ms,
        "facts": outcome.as_ref().map(|outcome| &outcome.facts),
        "diagnostic": outcome.as_ref().and_then(|outcome| outcome.diagnostic.as_ref()),
        "methodCoverage": method_coverage(session.model.provider)?,
        "receipts": {
            "runCount": receipts.run_count,
            "runsTruncated": receipts.run_count > runs.len() as u64,
            "effectCount": receipts.effect_count,
            "effectsTruncated": receipts.effect_count > shown_effects,
            "runs": runs,
        },
        "nativeToolErrors": errors,
    }))
}

fn method_coverage(provider: Provider) -> Result<serde_json::Value> {
    if !Provider::SUPPORTED.contains(&provider) {
        return Ok(serde_json::json!({
            "provider": provider,
            "status": "retired",
            "executionAvailable": false,
        }));
    }
    let catalog = crate::provider_methods::describe(provider)?;
    let mut statuses = std::collections::BTreeMap::new();
    for method in catalog["methods"]
        .as_array()
        .ok_or(Error::Protocol("provider method inventory"))?
    {
        let status = method["status"]
            .as_str()
            .ok_or(Error::Protocol("provider method status"))?;
        *statuses.entry(status.to_owned()).or_insert(0u64) += 1;
    }
    Ok(serde_json::json!({
        "scope": catalog["scope"],
        "arbitraryProviderCalls": catalog["arbitraryProviderCalls"],
        "coverageMeaning": catalog["coverageMeaning"],
        "codexSchemaSha256": catalog["codexSchemaSha256"],
        "claudeSdkVersion": catalog["claudeSdkVersion"],
        "total": catalog["methods"].as_array().map(Vec::len),
        "statuses": statuses,
    }))
}

pub fn statuses() -> Vec<NativeBackendStatus> {
    Provider::SUPPORTED.into_iter().map(status).collect()
}

pub fn require_execution(config: &Config, requirements: TaskRequirements) -> Result<()> {
    if requirements.native_execution
        && (!cfg!(target_os = "macos") || config.native_execution.is_empty())
    {
        return Err(Error::Unavailable(NOT_QUALIFIED));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_grants_are_closed_exact_and_absent_from_legacy_config() {
        let legacy = serde_json::to_value(Config::default()).unwrap();
        assert!(legacy.get("native_execution").is_none());
        let scope = NativeScope {
            workspace: PathBuf::from("/tmp/project"),
            providers: vec![Provider::Claude, Provider::Codex],
            github_credentials: false,
            read_only_roots: vec![],
            git_metadata: vec![],
        };
        let config = NativeConfig {
            scopes: vec![scope.clone()],
        };
        config.validate().unwrap();
        assert!(
            config
                .scope(Path::new("/tmp/project"), Provider::Claude)
                .is_some()
        );
        assert!(
            config
                .scope(Path::new("/tmp/project"), Provider::Codex)
                .is_some()
        );
        assert!(
            config
                .scope(Path::new("/tmp/project"), Provider::Devin)
                .is_none()
        );
        let mut retired = config.clone();
        retired.scopes[0].providers = vec![Provider::Devin];
        assert!(retired.validate().is_err());
        assert!(!status(Provider::Devin).implemented);
        assert!(
            config
                .scope(Path::new("/tmp/project/nested"), Provider::Claude)
                .is_none()
        );
        let mut foreign = serde_json::to_value(&scope).unwrap();
        foreign["network"] = serde_json::json!("unrestricted");
        assert!(serde_json::from_value::<NativeScope>(foreign).is_err());
        let mut duplicate = config.clone();
        duplicate.scopes.push(scope.clone());
        assert!(duplicate.validate().is_err());
        let mut duplicate_provider = config.clone();
        duplicate_provider.scopes[0]
            .providers
            .push(Provider::Claude);
        assert!(duplicate_provider.validate().is_err());
        let mut relative = config;
        relative.scopes[0].workspace = PathBuf::from("project");
        assert!(relative.validate().is_err());
    }

    #[test]
    fn native_grants_cannot_cover_private_state_or_its_ancestors() {
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let state = crate::private::directory(&base.join("state")).unwrap();
        let workspace = crate::private::directory(&base.join("workspace")).unwrap();
        let mut scope = NativeScope {
            workspace,
            providers: Provider::SUPPORTED.to_vec(),
            github_credentials: false,
            read_only_roots: vec![],
            git_metadata: vec![],
        };
        validate_grant(&scope, &state).unwrap();
        for path in [state.clone(), base.clone()] {
            scope.read_only_roots = vec![path.clone()];
            assert!(validate_grant(&scope, &state).is_err());
            scope.read_only_roots.clear();
            scope.git_metadata = vec![path];
            assert!(validate_grant(&scope, &state).is_err());
            scope.git_metadata.clear();
        }
        scope.workspace = state;
        assert!(validate_grant(&scope, &base.join("state")).is_err());
    }

    #[test]
    fn native_fixture_permits_bind_owner_arguments_and_one_call() {
        use xcb_core::{
            Provider,
            models::{Mode, ModelChoice},
        };
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let workspace = crate::private::directory(&base.join("workspace")).unwrap();
        let account = store
            .add_account(Provider::Codex, "Fixture", crate::now_ms(), None)
            .unwrap();
        let model = ModelChoice {
            provider: Provider::Codex,
            id: xcb_core::Id::new("fixture").unwrap(),
            label: "Fixture".into(),
            mode: Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: crate::now_ms(),
        };
        let session = store
            .create_session(&account.id, model, &workspace, crate::now_ms())
            .unwrap();
        let scope = NativeScope {
            workspace: workspace.clone(),
            providers: vec![Provider::Codex],
            github_credentials: false,
            read_only_roots: vec![],
            git_metadata: vec![],
        };
        let arguments =
            serde_json::json!({"argv":["/bin/true"],"cwd":".","timeoutMs":1000,"network":"https"});
        let path = qualification_permit(&store, &session, scope, &arguments).unwrap();
        let run = store
            .prepare_run(&session.id, session.revision, crate::now_ms())
            .unwrap();
        let mut altered = arguments.clone();
        altered["argv"] = serde_json::json!(["/bin/false"]);
        assert!(matches!(
            admit(&store, &run, &workspace, &altered, "first", false),
            Err(Error::Unavailable(
                "native qualification permit does not authorize this exact command"
            ))
        ));
        let permit: QualificationPermit =
            serde_json::from_slice(&crate::private::read(&path, 32768).unwrap()).unwrap();
        assert!(permit.consumed.is_none());
        let reordered: serde_json::Value = serde_json::from_str(
            r#"{"network":"https","timeoutMs":1000,"cwd":".","argv":["/bin/true"]}"#,
        )
        .unwrap();
        assert_eq!(reordered, arguments);
        assert!(admit(&store, &run, &workspace, &reordered, "first", true).is_err());
        let bytes = crate::private::read(&path, 32768).unwrap();
        let mut permit: QualificationPermit = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(permit.consumed, Some((run.id.clone(), "first".into())));
        assert!(matches!(
            admit(&store, &run, &workspace, &arguments, "second", false),
            Err(Error::Unavailable(
                "native qualification permit does not authorize this exact command"
            ))
        ));
        permit.owner_instance = "other".into();
        crate::private::replace(
            &path,
            &serde_json::to_vec(&permit).unwrap(),
            &crate::digest(bytes),
        )
        .unwrap();
        assert!(matches!(
            admit(&store, &run, &workspace, &arguments, "first", false),
            Err(Error::Unavailable(
                "native qualification permit does not authorize this exact command"
            ))
        ));
    }

    #[test]
    fn session_inspection_reports_custody_effects_and_method_coverage_without_a_provider() {
        use xcb_core::{
            Id, Provider,
            models::{Mode, ModelChoice},
            session::State,
        };

        let directory = tempfile::tempdir().unwrap();
        let root = xcb_core::canonical(directory.path()).unwrap();
        let workspace = crate::private::directory(&root.join("workspace")).unwrap();
        let store = Store::open(&root.join("state")).unwrap();
        let account = store
            .add_account(Provider::Codex, "Fixture", 1, None)
            .unwrap();
        let model = ModelChoice {
            provider: Provider::Codex,
            id: Id::new("fixture").unwrap(),
            label: "Fixture".into(),
            mode: Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: 1,
        };
        let session = store
            .create_session(&account.id, model, &workspace, 1)
            .unwrap();
        store
            .require_session_capabilities(
                &session.id,
                TaskRequirements {
                    native_execution: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let session = store.session(&session.id).unwrap().unwrap();
        let run = store.prepare_run(&session.id, session.revision, 2).unwrap();
        let custody = crate::command::CommandCustody {
            version: 1,
            command_id: Id::new("cmd_synthetic").unwrap(),
            run_id: run.id.clone(),
            workspace_id: "a".repeat(64),
            snapshot_sha256: "b".repeat(64),
            request_sha256: "c".repeat(64),
            backend_sha256: "d".repeat(64),
            boot_id: "00000000-0000-0000-0000-000000000001".into(),
        };
        store.record_command_custody(&run, &custody).unwrap();
        store.mark_capability_starting(&run, "browser").unwrap();
        store
            .mark_capability_spawned(&run, "browser", i32::MAX as u32)
            .unwrap();
        let run = store.run(&run.id).unwrap().unwrap();
        store
            .begin_tool(
                &run,
                "native-call",
                "workspace_native_exec",
                "synthetic-digest",
            )
            .unwrap();

        let inspection = inspect_session(&store, &session.id).unwrap();
        assert_eq!(inspection["provider"], Provider::Codex.as_str());
        assert_eq!(inspection["session"], session.id.as_str());
        assert_eq!(inspection["requirements"]["native_execution"], true);
        assert_eq!(inspection["receipts"]["runCount"], 1);
        assert_eq!(inspection["receipts"]["effectCount"], 1);
        assert_eq!(
            inspection["receipts"]["runs"][0]["id"].as_str(),
            Some(run.id.as_str())
        );
        assert_eq!(
            inspection["receipts"]["runs"][0]["leaseHeld"].as_bool(),
            Some(true)
        );
        assert_eq!(
            inspection["receipts"]["runs"][0]["owner"]["pid"],
            std::process::id()
        );
        assert_eq!(inspection["receipts"]["runs"][0]["owner"]["alive"], true);
        assert_eq!(
            inspection["receipts"]["runs"][0]["toolEffects"][0],
            serde_json::json!({
                "call": "native-call",
                "operation": "workspace_native_exec",
                "inputDigest": "synthetic-digest",
                "settled": false,
            })
        );
        assert_eq!(
            inspection["receipts"]["runs"][0]["commandCustody"]["commandId"],
            "cmd_synthetic"
        );
        assert_eq!(
            inspection["receipts"]["runs"][0]["capabilityProcesses"]["browser"],
            i32::MAX
        );
        assert_eq!(
            inspection["methodCoverage"]["codexSchemaSha256"],
            crate::codex::SCHEMA_SHA256
        );
        assert_eq!(inspection["methodCoverage"]["total"], 262);
        assert_eq!(
            inspection["methodCoverage"]["coverageMeaning"],
            "accountedForNotAllEnabled"
        );
        assert_eq!(inspection["nativeToolErrors"], serde_json::json!([]));

        store.clear_command_custody(&run, &custody).unwrap();
        store.clear_capability_custody(&run, "browser").unwrap();
        store.settle_tool(&run, "native-call").unwrap();
        store.settle(&run, State::Idle, 3).unwrap();
        let inspection = inspect_session(&store, &session.id).unwrap();
        let receipt = &inspection["receipts"]["runs"][0];
        assert_eq!(receipt["leaseHeld"], false);
        assert_eq!(receipt["toolEffects"][0]["settled"], true);
        assert!(receipt["commandCustody"].is_null());
        assert_eq!(receipt["capabilityProcesses"], serde_json::json!({}));
    }

    #[test]
    fn native_status_reports_bounded_local_records_and_exact_filters() {
        use xcb_core::{
            Id, Provider,
            models::{Mode, ModelChoice},
            session::State,
        };

        let directory = tempfile::tempdir().unwrap();
        let root = xcb_core::canonical(directory.path()).unwrap();
        let workspace = crate::private::directory(&root.join("workspace")).unwrap();
        let store = Store::open(&root.join("state")).unwrap();
        let model = |provider: Provider| ModelChoice {
            provider,
            id: Id::new(format!("fixture-{provider}")).unwrap(),
            label: "Fixture".into(),
            mode: Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: 1,
        };
        let codex = store
            .add_account(Provider::Codex, "Fixture", 1, None)
            .unwrap();
        let claude = store
            .add_account(Provider::Claude, "Fixture", 1, None)
            .unwrap();
        let codex_session = store
            .create_session(&codex.id, model(Provider::Codex), &workspace, 1)
            .unwrap();
        let claude_session = store
            .create_session(&claude.id, model(Provider::Claude), &workspace, 2)
            .unwrap();
        let run = store
            .prepare_run(&codex_session.id, codex_session.revision, 3)
            .unwrap();
        let custody = crate::command::CommandCustody {
            version: 1,
            command_id: Id::new("cmd_synthetic").unwrap(),
            run_id: run.id.clone(),
            workspace_id: "a".repeat(64),
            snapshot_sha256: "b".repeat(64),
            request_sha256: "c".repeat(64),
            backend_sha256: "d".repeat(64),
            boot_id: "00000000-0000-0000-0000-000000000001".into(),
        };
        store.record_command_custody(&run, &custody).unwrap();
        store.mark_capability_starting(&run, "browser").unwrap();
        store
            .mark_capability_spawned(&run, "browser", i32::MAX as u32)
            .unwrap();
        let run = store.run(&run.id).unwrap().unwrap();
        store
            .begin_tool(
                &run,
                "native-call",
                "workspace_native_exec",
                "synthetic-digest",
            )
            .unwrap();

        let status = status_snapshot(&store, StatusQuery::default()).unwrap();
        assert_eq!(status["inspection"]["localOnly"], true);
        assert_eq!(status["inspection"]["providerAttached"], false);
        assert_eq!(status["inspection"]["providerProcessesStarted"], 0);
        assert_eq!(status["inspection"]["promptSubmitted"], false);
        assert_eq!(status["totals"]["accounts"], 2);
        assert_eq!(status["totals"]["sessions"], 2);
        assert_eq!(status["totals"]["runs"], 1);
        assert_eq!(status["totals"]["toolEffects"], 1);
        assert_eq!(status["totals"]["leases"], 1);
        assert_eq!(status["totals"]["unsettledEffects"], 1);
        assert_eq!(status["totals"]["unlinkedToolEffects"], 0);
        assert_eq!(status["accounts"]["matched"], 2);
        assert_eq!(status["sessions"]["matched"], 2);
        assert_eq!(status["runs"]["matched"], 1);
        assert_eq!(status["effects"]["matched"], 1);
        let account = status["accounts"]["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["id"] == codex.id.as_str())
            .unwrap();
        assert_eq!(account["provider"], "codex");
        assert_eq!(account["leaseHeld"], true);
        assert_eq!(account["busy"], true);
        assert_eq!(account["activeRuns"], 1);
        assert_eq!(account["credentialsPresent"], false);
        let session = status["sessions"]["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["id"] == codex_session.id.as_str())
            .unwrap();
        assert!(session.get("title").is_none());
        assert_eq!(session["provider"], "codex");
        assert_eq!(session["state"], "working");
        assert_eq!(session["leaseHeld"], true);
        assert_eq!(session["pendingCommandCustody"], true);
        assert_eq!(session["capabilityProcessCount"], 1);
        assert_eq!(session["effects"]["unsettled"], 1);
        let run_record = &status["runs"]["records"][0];
        assert_eq!(run_record["id"], run.id.as_str());
        assert_eq!(run_record["provider"], "codex");
        assert_eq!(run_record["leaseHeld"], true);
        assert_eq!(run_record["owner"]["alive"], true);
        assert_eq!(run_record["commandCustody"]["commandId"], "cmd_synthetic");
        assert_eq!(run_record["effects"]["unsettled"], 1);
        let effect = &status["effects"]["records"][0];
        assert_eq!(effect["session"], codex_session.id.as_str());
        assert_eq!(effect["run"], run.id.as_str());
        assert_eq!(effect["settled"], false);
        let serialized = serde_json::to_string(&status).unwrap();
        for forbidden in [
            "providerHome",
            "provider_home",
            "credentialSource",
            "authToken",
            "accessToken",
            "refreshToken",
            "rawPrompt",
            "agentPrompt",
            "argv",
        ] {
            assert!(!serialized.contains(forbidden), "{forbidden}");
        }

        let claude_only = status_snapshot(
            &store,
            StatusQuery {
                provider: Some(Provider::Claude),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(claude_only["accounts"]["matched"], 1);
        assert_eq!(claude_only["sessions"]["matched"], 1);
        assert_eq!(claude_only["runs"]["matched"], 0);
        assert_eq!(claude_only["effects"]["matched"], 0);

        let account_only = status_snapshot(
            &store,
            StatusQuery {
                account: Some(codex.id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(account_only["accounts"]["matched"], 1);
        assert_eq!(account_only["sessions"]["matched"], 1);
        assert_eq!(account_only["runs"]["matched"], 1);

        let session_only = status_snapshot(
            &store,
            StatusQuery {
                session: Some(codex_session.id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(session_only["accounts"]["matched"], 1);
        assert_eq!(session_only["sessions"]["matched"], 1);
        assert_eq!(session_only["runs"]["matched"], 1);
        assert_eq!(session_only["effects"]["matched"], 1);

        let working = status_snapshot(
            &store,
            StatusQuery {
                state: Some(State::Working),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(working["sessions"]["matched"], 1);
        assert_eq!(working["runs"]["matched"], 1);

        for query in [
            StatusQuery {
                has_lease: true,
                ..Default::default()
            },
            StatusQuery {
                unsettled_effects: true,
                ..Default::default()
            },
            StatusQuery {
                pending_command_custody: true,
                ..Default::default()
            },
        ] {
            let status = status_snapshot(&store, query).unwrap();
            assert_eq!(status["accounts"]["matched"], 1);
            assert_eq!(status["sessions"]["matched"], 1);
            assert_eq!(status["runs"]["matched"], 1);
        }

        let first = status_snapshot(
            &store,
            StatusQuery {
                limit: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(first["sessions"]["matched"], 2);
        assert_eq!(first["sessions"]["returned"], 1);
        assert_eq!(first["sessions"]["truncated"], true);
        assert_eq!(first["sessions"]["nextCursor"], 1);
        let first_id = first["sessions"]["records"][0]["id"].clone();
        let second = status_snapshot(
            &store,
            StatusQuery {
                limit: 1,
                cursor: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(second["sessions"]["cursor"], 1);
        assert_eq!(second["sessions"]["returned"], 1);
        assert_ne!(second["sessions"]["records"][0]["id"], first_id);
        assert!(
            status["sessions"]["records"]
                .as_array()
                .unwrap()
                .iter()
                .any(|record| record["id"] == claude_session.id.as_str())
        );
        assert!(
            status_snapshot(
                &store,
                StatusQuery {
                    limit: 0,
                    ..Default::default()
                },
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn native_status_reports_managed_schedules_and_projects_without_a_provider() {
        let directory = tempfile::tempdir().unwrap();
        let root = xcb_core::canonical(directory.path()).unwrap();
        let state = root.join("state");
        let workspace = crate::private::directory(&root.join("work")).unwrap();
        let managed = crate::managed::ManagedStore::open(&state).unwrap();
        let store = Store::open(&state).unwrap();
        let conversation = managed.create_conversation(&workspace).await.unwrap();
        let now = crate::now_ms();
        let schedule = managed
            .create_schedule(&conversation.id, "sweep".into(), 60_000, now)
            .await
            .unwrap();
        let policy = managed
            .configure_project_policy_in(
                &workspace,
                None,
                "Maintain the project".into(),
                8,
                now + 7_200_000,
                Some(Provider::Codex),
                2,
                4,
            )
            .unwrap();

        let status = status_snapshot(&store, StatusQuery::default()).unwrap();
        assert_eq!(status["inspection"]["providerProcessesStarted"], 0);
        assert_eq!(status["managed"]["available"], true);
        assert!(status["managed"].get("schedules").is_none());
        assert!(status["managed"].get("projects").is_none());
        assert_eq!(status["schedules"]["matched"], 1);
        let record = &status["schedules"]["records"][0];
        assert_eq!(record["id"], schedule.id.as_str());
        assert_eq!(record["kind"], "prompt");
        assert_eq!(record["enabled"], true);
        assert_eq!(record["dueNow"], true);
        assert_eq!(record["workspace"], policy.workspace);
        assert_eq!(record["blocker"], serde_json::Value::Null);
        assert!(record.get("prompt").is_none());
        assert_eq!(status["projects"]["matched"], 1);
        let project = &status["projects"]["records"][0];
        assert_eq!(project["workspace"], policy.workspace);
        assert_eq!(project["maxActive"], 2);
        assert_eq!(project["maxPerHour"], 4);
        assert_eq!(project["lanes"], 0);
        assert_eq!(project["schedules"], 1);
        assert_eq!(project["admissionsLastHour"], 0);
        assert_eq!(project["requiredProvider"], "codex");

        // The exact workspace filter keeps the herd and its schedules.
        let filtered = status_snapshot(
            &store,
            StatusQuery {
                workspace: Some(policy.workspace.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(filtered["schedules"]["matched"], 1);
        assert_eq!(filtered["projects"]["matched"], 1);
        let elsewhere = crate::private::directory(&root.join("other")).unwrap();
        let filtered = status_snapshot(
            &store,
            StatusQuery {
                workspace: Some(elsewhere.to_str().unwrap().to_owned()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(filtered["schedules"]["matched"], 0);
        assert_eq!(filtered["projects"]["matched"], 0);
    }

    #[test]
    fn session_inspection_marks_effects_hidden_by_run_truncation() {
        use xcb_core::{
            Id, Provider,
            models::{Mode, ModelChoice},
            session::State,
        };

        let directory = tempfile::tempdir().unwrap();
        let root = xcb_core::canonical(directory.path()).unwrap();
        let workspace = crate::private::directory(&root.join("workspace")).unwrap();
        let store = Store::open(&root.join("state")).unwrap();
        let account = store
            .add_account(Provider::Codex, "Fixture", 1, None)
            .unwrap();
        let model = ModelChoice {
            provider: Provider::Codex,
            id: Id::new("fixture").unwrap(),
            label: "Fixture".into(),
            mode: Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: 1,
        };
        let session = store
            .create_session(&account.id, model, &workspace, 1)
            .unwrap();
        for index in 0..17u64 {
            let current = store.session(&session.id).unwrap().unwrap();
            let run = store
                .prepare_run(&session.id, current.revision, 2 + index)
                .unwrap();
            if index == 0 {
                store
                    .begin_tool(&run, "native-call", "workspace_native_exec", "digest")
                    .unwrap();
                store.settle_tool(&run, "native-call").unwrap();
            }
            store.settle(&run, State::Idle, 20 + index).unwrap();
        }

        let inspection = inspect_session(&store, &session.id).unwrap();
        assert_eq!(inspection["receipts"]["runCount"], 17);
        assert_eq!(inspection["receipts"]["runsTruncated"], true);
        assert_eq!(inspection["receipts"]["effectCount"], 1);
        assert_eq!(inspection["receipts"]["effectsTruncated"], true);
        assert!(
            inspection["receipts"]["runs"]
                .as_array()
                .unwrap()
                .iter()
                .all(|run| run["toolEffects"].as_array().unwrap().is_empty())
        );
    }

    #[test]
    fn native_candidates_cover_every_provider_without_claiming_activation() {
        let statuses = statuses();
        assert_eq!(statuses.len(), Provider::SUPPORTED.len());
        for (provider, status) in Provider::SUPPORTED.into_iter().zip(statuses) {
            assert_eq!(status.provider, provider);
            assert_eq!(status.implemented, cfg!(target_os = "macos"));
            assert!(!status.qualified && !status.fallback_permitted);
            assert_eq!(status.required_cases, ACCEPTANCE_CASES);
        }
    }

    #[test]
    fn legacy_execution_is_unchanged_but_native_requests_never_fall_back() {
        assert!(require_execution(&Config::default(), TaskRequirements::default()).is_ok());
        for provider in Provider::SUPPORTED {
            let requirements = TaskRequirements {
                native_execution: true,
                ..Default::default()
            };
            assert!(requirements.allows(provider));
            assert!(matches!(
                require_execution(
                    &Config::default(),
                    requirements.merge(TaskRequirements::default())
                ),
                Err(Error::Unavailable(NOT_QUALIFIED))
            ));
        }
    }

    #[tokio::test]
    async fn native_requirement_is_persisted_and_refused_before_any_run_is_prepared() {
        use crate::{config::Config, runner, store::Store};
        use std::sync::Arc;
        use tokio::sync::watch;
        use xcb_core::{
            Id,
            models::{Mode, ModelChoice},
            session::{Message, Role},
        };

        let directory = tempfile::tempdir().unwrap();
        let root = xcb_core::canonical(directory.path()).unwrap();
        let workspace = crate::private::directory(&root.join("workspace")).unwrap();
        let store = Arc::new(Store::open(&root.join("state")).unwrap());
        for provider in Provider::SUPPORTED {
            let account = store.add_account(provider, "Fixture", 1, None).unwrap();
            let model = ModelChoice {
                provider,
                id: Id::new("fixture").unwrap(),
                label: "Fixture".into(),
                mode: Mode::Fixed,
                effort: None,
                resolved: None,
                observed_at_ms: 1,
            };
            let session = store
                .create_managed_session(
                    &account.id,
                    model,
                    &workspace,
                    1,
                    &Id::new(format!("task_{provider}")).unwrap(),
                )
                .unwrap();
            store
                .require_session_capabilities(
                    &session.id,
                    TaskRequirements {
                        native_execution: true,
                        ..Default::default()
                    },
                )
                .unwrap();
            store
                .require_session_capabilities(&session.id, TaskRequirements::default())
                .unwrap();
            let session = store.session(&session.id).unwrap().unwrap();
            assert!(session.requirements.native_execution);
            let (_, cancel) = watch::channel(false);
            let result = runner::run(
                store.clone(),
                runner::RunInput {
                    session,
                    message: Message {
                        id: Id::new("message_fixture").unwrap(),
                        role: Role::User,
                        text: "Run native tools".into(),
                        at_ms: 1,
                        attachments: vec![],
                        provenance: None,
                    },
                    config: Config::default(),
                    pane_generation: false,
                },
                cancel,
                Arc::new(|_| {}),
            )
            .await;
            assert!(matches!(result, Err(Error::Unavailable(NOT_QUALIFIED))));
            assert!(store.unsettled_runs().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn native_routing_and_dry_run_do_not_select_broker_only_accounts() {
        use crate::{config::Config, route, routing, store::Store};
        use std::{collections::BTreeSet, sync::Arc};
        use tokio::sync::watch;

        let directory = tempfile::tempdir().unwrap();
        let root = xcb_core::canonical(directory.path()).unwrap();
        let workspace = crate::private::directory(&root.join("workspace")).unwrap();
        let store = Arc::new(Store::open(&root.join("state")).unwrap());
        let requirements = TaskRequirements {
            native_execution: true,
            ..Default::default()
        };
        let excluded = BTreeSet::new();
        let accounts = BTreeSet::new();
        for provider in Provider::SUPPORTED {
            let result = routing::smart_route(
                &store,
                &Config::default(),
                routing::RouteRequest {
                    requirements,
                    task: "Run native tools",
                    required_provider: Some(provider),
                    preferred_provider: None,
                    required_model: None,
                    excluded_routes: &excluded,
                    excluded_accounts: &accounts,
                    account: None,
                },
            )
            .await;
            assert!(matches!(result, Err(Error::Unavailable(NOT_QUALIFIED))));
            for dry_run in [false, true] {
                let request = route::RouteTaskRequest::parse(
                    &serde_json::to_vec(&serde_json::json!({
                        "version":1,
                        "workspace":workspace,
                        "task":"Run native tools",
                        "provider":provider,
                        "requirements":{"native_execution":true},
                        "dryRun":dry_run,
                    }))
                    .unwrap(),
                )
                .unwrap();
                let (_, cancel) = watch::channel(false);
                let result =
                    route::dispatch(store.clone(), request, cancel, Arc::new(|_| {})).await;
                let failure = result.unwrap_err();
                assert_eq!(failure.code, route::RouteCode::Unavailable);
                assert_eq!(failure.joined, Some(true));
                assert_eq!(failure.effects, Some("none"));
                assert!(failure.session.is_none() && failure.route.is_none());
                assert!(store.sessions(10).unwrap().is_empty());
                assert!(store.unsettled_runs().unwrap().is_empty());
            }
        }
    }

    #[test]
    fn provider_specific_approval_contracts_do_not_alias_bypass() {
        assert_eq!(
            status(Provider::Codex).approval,
            NativeApproval::CodexAutoReview
        );
        assert_eq!(
            status(Provider::Claude).approval,
            NativeApproval::ClaudeAuto
        );
        assert_eq!(
            status(Provider::Devin).approval,
            NativeApproval::DevinProviderReview
        );
        assert_ne!(
            status(Provider::Devin).approval,
            status(Provider::Claude).approval
        );
    }
}
