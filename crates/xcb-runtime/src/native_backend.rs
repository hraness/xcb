use crate::{
    Error, Result,
    config::Config,
    private,
    process::Pin,
    store::{RunRecord, Store},
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use xcb_core::{Provider, session::TaskRequirements};

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
