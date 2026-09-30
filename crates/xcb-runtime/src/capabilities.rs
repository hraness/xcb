//! Owner-registered host MCP tools shared by every provider. Provider children
//! never inherit these servers' environment or credentials. The broker owns
//! initialization, a fixed inventory, effect settlement and process cleanup.
pub use crate::capability_bundle::CapabilityBundle;
use crate::{
    Error, Result, private,
    process::{StreamProcess, snapshot_pinned_executable},
    runner::LaunchArtifacts,
    store::{RunRecord, Store},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use xcb_core::policy::EffectState;

const MAX_SERVERS: usize = 8;
const MAX_TOOLS: usize = 128;
const MAX_INVENTORY_BYTES: usize = 256 * 1024;
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityFeature {
    Browser,
    Computer,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityTransport {
    #[default]
    Shared,
    CodexNative,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityServer {
    pub name: String,
    /// The browser bridge's selected xcb Claude account. No credential bytes
    /// are persisted here or exposed to provider tool descriptions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_account: Option<xcb_core::Id>,
    #[serde(default)]
    pub transport: CapabilityTransport,
    pub executable: PathBuf,
    pub sha256: String,
    /// Immutable owner snapshots for interpreted entrypoints and native helpers.
    #[serde(default)]
    pub bundles: Vec<CapabilityBundle>,
    #[serde(default)]
    pub args: Vec<String>,
    /// Variable names, never a copy of the caller's whole environment. Missing
    /// explicitly required variables fail before the process is launched.
    #[serde(default)]
    pub env: Vec<String>,
    /// Explicit owner-supplied nonsecret settings. Credentials belong in
    /// forwarded variables, not in this persisted configuration.
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    /// A connector manifest may admit only part of a server's inventory.
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    #[serde(default)]
    pub shutdown_tool: Option<String>,
    #[serde(default)]
    pub features: Vec<CapabilityFeature>,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
}

fn default_timeout() -> u64 {
    120_000
}

fn name_valid(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
}

fn env_name_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && !value.as_bytes()[0].is_ascii_digit()
        && ![
            "HOME",
            "USERPROFILE",
            "TMPDIR",
            "TMP",
            "TEMP",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "SYSTEMROOT",
        ]
        .contains(&value.to_ascii_uppercase().as_str())
}

impl CapabilityServer {
    pub fn validate(&self) -> Result<()> {
        if self.credential_account.is_some()
            && (self.name != crate::chrome_connector::SERVER_NAME
                || self.transport != CapabilityTransport::Shared)
        {
            return Err(xcb_core::Error::Invalid("host tool credential binding").into());
        }
        if !name_valid(&self.name, 64)
            || !self.name.as_bytes()[0].is_ascii_alphanumeric()
            || (self.transport == CapabilityTransport::CodexNative && self.name != "cua_repl")
            || !self.executable.is_absolute()
            || self.executable.as_os_str().len() > 4096
            || self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || self.args.len() > 64
            || self.bundles.len() > 4
            || self
                .bundles
                .iter()
                .map(|bundle| &bundle.root)
                .collect::<BTreeSet<_>>()
                .len()
                != self.bundles.len()
            || self
                .args
                .iter()
                .any(|value| value.len() > 4096 || value.contains('\0'))
            || self.args.iter().map(String::len).sum::<usize>() > 16 * 1024
            || self.env.len() > 32
            || self.env.iter().any(|name| !env_name_valid(name))
            || self.env.iter().collect::<BTreeSet<_>>().len() != self.env.len()
            || self.environment.len() > 32
            || self.environment.iter().any(|(name, value)| {
                !env_name_valid(name)
                    || self.env.contains(name)
                    || value.len() > 4096
                    || value.contains('\0')
            })
            || self.tools.as_ref().is_some_and(|tools| {
                tools.is_empty()
                    || tools.len() > MAX_TOOLS
                    || tools.iter().any(|tool| !name_valid(tool, 128))
                    || tools.iter().collect::<BTreeSet<_>>().len() != tools.len()
            })
            || self.shutdown_tool.as_ref().is_some_and(|tool| {
                !name_valid(tool, 128)
                    || self
                        .tools
                        .as_ref()
                        .is_some_and(|tools| !tools.contains(tool))
            })
            || self.features.len() > 2
            || self.features.iter().collect::<BTreeSet<_>>().len() != self.features.len()
            || !(1_000..=600_000).contains(&self.timeout_ms)
        {
            return Err(xcb_core::Error::Invalid("host tool server configuration").into());
        }
        self.bundles.iter().try_for_each(CapabilityBundle::validate)
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CapabilityConfig {
    pub servers: Vec<CapabilityServer>,
}
impl CapabilityConfig {
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }
    pub fn validate(&self) -> Result<()> {
        if self.servers.len() > MAX_SERVERS
            || self
                .servers
                .iter()
                .map(|server| &server.name)
                .collect::<BTreeSet<_>>()
                .len()
                != self.servers.len()
            || serde_json::to_vec(self)?.len() > 32 * 1024
        {
            return Err(xcb_core::Error::Invalid("host tool server inventory").into());
        }
        self.servers.iter().try_for_each(CapabilityServer::validate)
    }
}

struct Server {
    process: StreamProcess,
    artifacts: LaunchArtifacts,
    tools: Option<Vec<Value>>,
    next_id: u64,
    pending_call: bool,
    native_pending: BTreeMap<String, NativeCall>,
    #[cfg(unix)]
    native_calls: usize,
    #[cfg(unix)]
    native_metadata: Option<Value>,
    #[cfg(unix)]
    native_turn_context: Option<Value>,
    #[cfg(unix)]
    native_activity: bool,
}

#[cfg_attr(not(unix), allow(dead_code))]
struct NativeCall {
    receipt: String,
    tool: String,
    input_digest: String,
    submitted: bool,
}

pub struct CallOutcome {
    pub result: Result<Value>,
    pub effects: EffectState,
}

pub struct CapabilityManager {
    config: CapabilityConfig,
    store: Arc<Store>,
    run: RunRecord,
    workspace: PathBuf,
    servers: Vec<Option<Server>>,
    effects: EffectState,
    stopped: bool,
    policy_denied: bool,
    chrome_tabs: std::collections::BTreeSet<u64>,
    credential_runs: Vec<Option<RunRecord>>,
    browser_bootstrap: bool,
}

impl CapabilityManager {
    pub fn new(
        config: CapabilityConfig,
        store: Arc<Store>,
        run: RunRecord,
        workspace: PathBuf,
    ) -> Result<Self> {
        config.validate()?;
        let servers = (0..config.servers.len()).map(|_| None).collect();
        let credential_runs = (0..config.servers.len()).map(|_| None).collect();
        Ok(Self {
            config,
            store,
            run,
            workspace,
            servers,
            effects: EffectState::None,
            stopped: false,
            policy_denied: false,
            chrome_tabs: Default::default(),
            credential_runs,
            browser_bootstrap: false,
        })
    }

    pub fn effects(&self) -> EffectState {
        if self
            .servers
            .iter()
            .flatten()
            .any(|server| server.pending_call || !server.native_pending.is_empty())
        {
            EffectState::Uncertain
        } else {
            self.effects
        }
    }

    pub fn policy_denied(&self) -> bool {
        self.policy_denied
    }

    fn index(&self, name: &str) -> Result<usize> {
        if self.stopped {
            return Err(Error::Conflict("host tools have stopped"));
        }
        if self.policy_denied {
            return Err(Error::Unavailable(
                "browser permission was denied; the task requires user attention",
            ));
        }
        self.config
            .servers
            .iter()
            .position(|server| server.name == name)
            .ok_or(Error::Unavailable("host tool server is not configured"))
    }

    async fn ensure_server(&mut self, index: usize) -> Result<()> {
        if self.config.servers[index].transport != CapabilityTransport::Shared {
            return Err(Error::Unavailable(
                "this tool server requires Codex native approvals",
            ));
        }
        if let Some(server) = &self.servers[index] {
            if server.pending_call || server.tools.is_none() {
                return Err(Error::Conflict("host tool server has an unsettled request"));
            }
            return Ok(());
        }
        self.launch_server(index)?;
        self.initialize_server(index).await
    }

    fn launch_server(&mut self, index: usize) -> Result<()> {
        if self.servers[index].is_some() || self.credential_runs[index].is_some() {
            return Err(Error::Conflict("host tool server already started"));
        }
        let config = &self.config.servers[index];
        // Registered server code/config cannot be supplied by the workspace
        // whose contents the provider can edit. Resolve existing symlinks too.
        reject_workspace_paths(config, &self.workspace)?;
        for bundle in &config.bundles {
            crate::capability_bundle::verify(
                &bundle.root,
                &bundle.sha256,
                crate::capability_bundle::BundleLimits::default(),
            )?;
        }
        let mut artifacts = LaunchArtifacts::create(self.store.root())?;
        let executable =
            snapshot_pinned_executable(&config.executable, &config.sha256, artifacts.path())?;
        let home = private::directory(&artifacts.path().join("home"))?;
        let temporary = private::directory(&artifacts.path().join("tmp"))?;
        let mut command = tokio::process::Command::new(executable);
        command
            .args(&config.args)
            .current_dir(&home)
            .env_clear()
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("TMPDIR", &temporary)
            .env("TMP", &temporary)
            .env("TEMP", &temporary)
            .env("XDG_CONFIG_HOME", &home)
            .env("XDG_CACHE_HOME", &temporary);
        #[cfg(windows)]
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", root);
        }
        for name in &config.env {
            let value = std::env::var_os(name).ok_or(Error::Unavailable(
                "configured host tool environment variable is missing",
            ))?;
            if value
                .to_str()
                .is_some_and(|value| points_into_workspace(value, &self.workspace))
            {
                return Err(Error::Unavailable(
                    "host tool configuration cannot load from the task workspace",
                ));
            }
            command.env(name, value);
        }
        command.envs(&config.environment);
        if config.name == crate::chrome_connector::SERVER_NAME {
            let account = config
                .credential_account
                .as_ref()
                .ok_or(Error::Unavailable(
                    "select an authenticated xcb Claude account for the browser connector",
                ))?;
            crate::chrome_connector::verify_credential_target(
                config,
                &self.store,
                &self.workspace,
            )?;
            if self.store.account(account)?.provider != xcb_core::Provider::Claude {
                return Err(Error::Unavailable(
                    "browser connector account must be a Claude account",
                ));
            }
            if account != &self.run.account {
                self.credential_runs[index] =
                    Some(self.store.prepare_probe(account, None, crate::now_ms())?);
            }
            let lease = self.credential_runs[index].as_ref().unwrap_or(&self.run);
            let token = (|| {
                self.store.require_authenticated_run(lease)?;
                crate::auth::token(&self.store, account)
            })();
            match token {
                Ok(token) => {
                    command
                        .env("CLAUDE_CODE_OAUTH_TOKEN", token.as_str())
                        .env("CLAUDE_CONFIG_DIR", &home);
                }
                Err(error) => {
                    if let Some(probe) = self.credential_runs[index].take() {
                        self.store.settle(
                            &probe,
                            xcb_core::session::State::Idle,
                            crate::now_ms(),
                        )?;
                    }
                    return Err(error);
                }
            }
            if let Some(probe) = &self.credential_runs[index] {
                self.store.mark_capability_starting(probe, &config.name)?;
            }
        }
        // Persist a starting marker first: a crash between spawn and pid
        // publication must never make recovery mistake this for no process.
        if let Err(error) = self.store.mark_capability_starting(&self.run, &config.name) {
            if let Some(probe) = self.credential_runs[index].take() {
                self.store.clear_capability_custody(&probe, &config.name)?;
                self.store
                    .settle(&probe, xcb_core::session::State::Idle, crate::now_ms())?;
            }
            return Err(error);
        }
        artifacts.retain_before_launch();
        let process = match StreamProcess::spawn(command) {
            Ok(process) => process,
            Err(error @ Error::LaunchNotStarted(_)) => {
                self.store
                    .clear_capability_custody(&self.run, &config.name)?;
                if let Some(probe) = self.credential_runs[index].take() {
                    self.store.clear_capability_custody(&probe, &config.name)?;
                    self.store
                        .settle(&probe, xcb_core::session::State::Idle, crate::now_ms())?;
                }
                artifacts.release_after_join(true, EffectState::None);
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        let pid = process.pid();
        self.servers[index] = Some(Server {
            process,
            artifacts,
            tools: None,
            next_id: 1,
            pending_call: false,
            native_pending: BTreeMap::new(),
            #[cfg(unix)]
            native_calls: 0,
            #[cfg(unix)]
            native_metadata: None,
            #[cfg(unix)]
            native_turn_context: None,
            #[cfg(unix)]
            native_activity: false,
        });
        self.store
            .mark_capability_spawned(&self.run, &config.name, pid)?;
        if self.browser_bootstrap {
            self.run = self.store.mark_spawned(&self.run, pid)?;
        }
        if let Some(probe) = &self.credential_runs[index] {
            self.store
                .mark_capability_spawned(probe, &config.name, pid)?;
            self.credential_runs[index] = Some(self.store.mark_spawned(probe, pid)?);
        }
        Ok(())
    }

    async fn initialize_server(&mut self, index: usize) -> Result<()> {
        let result = self
            .rpc(
                index,
                "initialize",
                json!({
                    "protocolVersion":"2024-11-05", "capabilities":{},
                    "clientInfo":{"name":"xcb","version":env!("CARGO_PKG_VERSION")}
                }),
            )
            .await?;
        if !matches!(
            result.get("protocolVersion").and_then(Value::as_str),
            Some("2024-11-05" | "2025-03-26" | "2025-06-18")
        ) || !result
            .get("capabilities")
            .and_then(|value| value.get("tools"))
            .is_some_and(Value::is_object)
        {
            return Err(Error::Protocol("host tool server initialization"));
        }
        self.servers[index]
            .as_mut()
            .expect("started server")
            .process
            .send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await?;
        let result = self.rpc(index, "tools/list", json!({})).await?;
        self.admit_inventory(index, result)?;
        Ok(())
    }

    fn admit_inventory(&mut self, index: usize, result: Value) -> Result<Value> {
        let mut tools = validate_inventory(result)?;
        if let Some(allowed) = &self.config.servers[index].tools {
            if allowed
                .iter()
                .any(|name| !tools.iter().any(|tool| tool["name"] == *name))
            {
                return Err(Error::Protocol("configured host tool is missing"));
            }
            tools.retain(|tool| allowed.iter().any(|name| tool["name"] == *name));
        }
        if self.config.servers[index]
            .shutdown_tool
            .as_ref()
            .is_some_and(|name| !tools.iter().any(|tool| tool["name"] == *name))
        {
            return Err(Error::Protocol("host tool shutdown operation is missing"));
        }
        self.servers[index]
            .as_mut()
            .ok_or(Error::Protocol("host tool server absent"))?
            .tools = Some(tools.clone());
        Ok(json!({"tools":tools}))
    }

    #[cfg(unix)]
    fn native_index(&self, name: &str) -> Result<usize> {
        let index = self.index(name)?;
        if self.config.servers[index].transport != CapabilityTransport::CodexNative {
            return Err(Error::Unavailable(
                "host tool server is not a native Codex connector",
            ));
        }
        Ok(index)
    }

    #[cfg(unix)]
    pub(crate) async fn native_open(&mut self, name: &str) -> Result<()> {
        let index = self.native_index(name)?;
        self.launch_server(index)
    }

    #[cfg(unix)]
    pub(crate) async fn native_send(&mut self, name: &str, frame: &Value) -> Result<()> {
        let index = self.native_index(name)?;
        let server = self.servers[index]
            .as_mut()
            .ok_or(Error::Protocol("host tool server absent"))?;
        let mut frame = frame.clone();
        if frame["method"] == "tools/call" {
            let id = serde_json::to_string(&frame["id"])?;
            let intent = server
                .native_pending
                .get_mut(&id)
                .ok_or(Error::Conflict("native host tool has no durable intent"))?;
            if intent.submitted
                || frame["params"]["name"] != intent.tool
                || crate::digest(serde_json::to_vec(&frame["params"]["arguments"])?)
                    != intent.input_digest
            {
                return Err(Error::Conflict(
                    "native host tool intent changed or was already submitted",
                ));
            }
            require_review(&mut frame["params"])?;
            intent.submitted = true;
            server.native_metadata = frame["params"].get("_meta").cloned();
            server.native_activity |= matches!(intent.tool.as_str(), "js" | "js_reset");
        }
        server.process.send(&frame).await
    }

    #[cfg(unix)]
    pub(crate) async fn native_frame(&mut self, name: &str) -> Result<Option<Vec<u8>>> {
        let index = self.native_index(name)?;
        self.servers[index]
            .as_mut()
            .ok_or(Error::Protocol("host tool server absent"))?
            .process
            .frame_bounded(MAX_FRAME_BYTES)
            .await
    }

    #[cfg(unix)]
    pub(crate) fn native_mark_initialized(
        &mut self,
        name: &str,
        inventory: Value,
    ) -> Result<Value> {
        let index = self.native_index(name)?;
        let hidden_lifecycle = inventory["tools"].as_array().is_some_and(|tools| {
            tools.iter().any(|tool| {
                tool["name"] == "turn_ended"
                    && tool
                        .pointer("/_meta/ui/visibility")
                        .and_then(Value::as_array)
                        .is_some_and(Vec::is_empty)
            })
        });
        let mut admitted = self.admit_inventory(index, inventory)?;
        if hidden_lifecycle
            && let Some(tool) = admitted["tools"]
                .as_array_mut()
                .and_then(|tools| tools.iter_mut().find(|tool| tool["name"] == "turn_ended"))
        {
            tool["_meta"] = json!({"ui":{"visibility":[]}});
        }
        Ok(admitted)
    }

    #[cfg(unix)]
    pub(crate) fn native_set_turn_context(
        &mut self,
        name: &str,
        session: &str,
        turn: &str,
    ) -> Result<()> {
        if [session, turn].iter().any(|value| {
            value.is_empty() || value.len() > 256 || value.chars().any(char::is_control)
        }) {
            return Err(Error::Protocol("native host turn identity"));
        }
        let index = self.native_index(name)?;
        let Some(server) = self.servers[index].as_mut() else {
            return Ok(());
        };
        server.native_turn_context =
            Some(json!({"hook_event_name":"Stop","session_id":session,"turn_id":turn}));
        Ok(())
    }

    /// Receipt is durable before a native proxy is allowed to submit a call.
    /// A missing response stays unsettled even after the server process exits.
    #[cfg(unix)]
    pub(crate) fn native_effect_begin(
        &mut self,
        name: &str,
        id: &str,
        tool: &str,
        arguments: &Value,
    ) -> Result<()> {
        let index = self.native_index(name)?;
        if id.len() > 1024
            || !matches!(
                serde_json::from_str::<Value>(id),
                Ok(Value::String(_) | Value::Number(_))
            )
        {
            return Err(Error::Protocol("native host tool request identity"));
        }
        let server = self.servers[index]
            .as_mut()
            .ok_or(Error::Protocol("host tool server absent"))?;
        if server.native_calls >= 128
            || server.native_pending.len() >= 32
            || server.native_pending.contains_key(id)
        {
            return Err(Error::Conflict(
                "duplicate or excessive native tool request",
            ));
        }
        if !arguments.is_object()
            || serde_json::to_vec(arguments)?.len() > xcb_core::MAX_TEXT_BYTES
            || !server
                .tools
                .as_ref()
                .is_some_and(|tools| tools.iter().any(|candidate| candidate["name"] == tool))
        {
            return Err(Error::Unavailable(
                "native host tool is not in the admitted inventory",
            ));
        }
        let receipt = format!("native.{name}.{}", &crate::digest(id)[..32]);
        let input_digest = crate::digest(serde_json::to_vec(arguments)?);
        if matches!(tool, "js" | "js_reset")
            && let Some(session) = &self.run.session
        {
            self.store.require_session_capabilities(
                session,
                xcb_core::session::TaskRequirements {
                    signed_in_browser: true,
                },
            )?;
        }
        self.store
            .begin_tool(&self.run, &receipt, tool, &input_digest)?;
        server.native_pending.insert(
            id.into(),
            NativeCall {
                receipt,
                tool: tool.into(),
                input_digest,
                submitted: false,
            },
        );
        server.native_calls += 1;
        Ok(())
    }

    #[cfg(unix)]
    pub(crate) fn native_effect_settle(
        &mut self,
        name: &str,
        id: &str,
        result: &Value,
    ) -> Result<Value> {
        let index = self.native_index(name)?;
        validate_result(result)?;
        let server = self.servers[index]
            .as_mut()
            .ok_or(Error::Protocol("host tool server absent"))?;
        let intent = server
            .native_pending
            .get(id)
            .ok_or(Error::Protocol("native host tool response identity"))?;
        if !intent.submitted {
            return Err(Error::Conflict(
                "native host tool response preceded submission",
            ));
        }
        let reply = if let Some(id) = &self.run.session {
            let session = self
                .store
                .session(id)?
                .ok_or(Error::Unavailable("session not found"))?;
            let output = crate::tool_output::prepare(self.store.root(), Ok(result.clone()), true);
            let message = xcb_core::session::Message {
                id: crate::new_id("tool"),
                role: xcb_core::session::Role::Tool,
                text: xcb_core::display_text(
                    &format!("{name}/{}: {}", intent.tool, output.text),
                    xcb_core::MAX_TEXT_BYTES,
                ),
                at_ms: crate::now_ms(),
                attachments: output.attachments,
                provenance: Some(xcb_core::session::MessageProvenance {
                    account: session.account,
                    model: session.model,
                    run: Some(self.run.id.clone()),
                }),
            };
            self.store
                .settle_tool_and_append(&self.run, &intent.receipt, id, &message)?;
            output.reply
        } else {
            self.store.settle_tool(&self.run, &intent.receipt)?;
            result.clone()
        };
        server.native_pending.remove(id);
        self.effects = EffectState::Settled;
        Ok(reply)
    }

    async fn rpc(&mut self, index: usize, method: &str, mut params: Value) -> Result<Value> {
        if method == "tools/call" {
            #[cfg(unix)]
            if params.get("_meta").is_none()
                && let Some(metadata) = self.servers[index]
                    .as_ref()
                    .and_then(|server| server.native_metadata.as_ref())
            {
                params["_meta"] = metadata.clone();
            }
            // CUA's standalone runtime otherwise defaults to execution without
            // its host reviewer. This applies to every configured server and
            // lifecycle call; owner metadata cannot disable the requirement.
            require_review(&mut params)?;
        }
        let timeout = Duration::from_millis(self.config.servers[index].timeout_ms);
        let server = self.servers[index]
            .as_mut()
            .ok_or(Error::Protocol("host tool server absent"))?;
        let id = server.next_id;
        server.next_id += 1;
        tokio::time::timeout(timeout, async {
            server
                .process
                .send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
                .await?;
            let mut received = 0usize;
            for _ in 0..128 {
                let bytes = server
                    .process
                    .frame_bounded(MAX_FRAME_BYTES)
                    .await?
                    .ok_or(Error::Protocol("host tool server closed output"))?;
                received += bytes.len();
                if received > 16 * 1024 * 1024 {
                    return Err(xcb_core::Error::Limit("host tool response bytes").into());
                }
                let frame: Value = serde_json::from_slice(&bytes)?;
                if frame.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
                    return Err(Error::Protocol("host tool JSON-RPC version"));
                }
                if frame.get("method").is_some() {
                    if let Some(request_id) = frame.get("id") {
                        // The host owns approvals. Sampling or elicitation is
                        // not silently authorized by accepting a tool server.
                        server
                            .process
                            .send(&json!({"jsonrpc":"2.0","id":request_id,
                            "error":{"code":-32601,"message":"Server requests are not supported"}}))
                            .await?;
                        return Err(Error::Protocol(
                            "host tool server requested unsupported host operation",
                        ));
                    }
                    if frame.get("method").and_then(Value::as_str)
                        == Some("notifications/tools/list_changed")
                    {
                        return Err(Error::Protocol("host tool inventory changed during run"));
                    }
                    continue;
                }
                if frame.get("id").and_then(Value::as_u64) != Some(id)
                    || frame.get("result").is_some() == frame.get("error").is_some()
                {
                    return Err(Error::Protocol("host tool response identity"));
                }
                if frame.get("error").is_some() {
                    return Err(Error::Unavailable("host tool server rejected the request"));
                }
                return Ok(frame["result"].clone());
            }
            Err(Error::Protocol("host tool notification limit"))
        })
        .await
        .map_err(|_| Error::Unavailable("host tool server timed out"))?
    }

    /// Launch only on an actual inventory request, never while rendering status
    /// or choosing a route. The provider receives schemas, not host paths/env.
    pub async fn list(&mut self, server: Option<&str>) -> Result<Value> {
        if self.stopped {
            return Err(Error::Conflict("host tools have stopped"));
        }
        let indices: Vec<usize> = match server {
            Some(name) => vec![self.index(name)?],
            None => (0..self.config.servers.len())
                .filter(|index| {
                    self.config.servers[*index].transport == CapabilityTransport::Shared
                })
                .collect(),
        };
        let mut inventory = Vec::new();
        for index in indices {
            self.ensure_server(index).await?;
            let config = &self.config.servers[index];
            inventory.push(json!({"server":config.name,"features":config.features,
                "tools":self.servers[index].as_ref().expect("initialized server").tools}));
        }
        let output = json!({"servers":inventory});
        if serde_json::to_vec(&output)?.len() > MAX_INVENTORY_BYTES {
            return Err(xcb_core::Error::Limit("host tool inventory bytes").into());
        }
        Ok(output)
    }

    pub async fn call(&mut self, server: &str, tool: &str, arguments: Value) -> CallOutcome {
        let mut submitted = false;
        let result = async {
            if !name_valid(tool, 128)
                || !arguments.is_object()
                || serde_json::to_vec(&arguments)?.len() > 256 * 1024
            {
                return Err(xcb_core::Error::Invalid("host tool arguments").into());
            }
            let index = self.index(server)?;
            if server == crate::chrome_connector::SERVER_NAME {
                crate::chrome_connector::validate_call(tool, &arguments)?;
            }
            self.ensure_server(index).await?;
            let state = self.servers[index].as_mut().expect("initialized server");
            if !state
                .tools
                .as_ref()
                .expect("inventory")
                .iter()
                .any(|candidate| candidate.get("name").and_then(Value::as_str) == Some(tool))
            {
                return Err(Error::Unavailable(
                    "host tool is not in the configured server inventory",
                ));
            }
            // Kept in self, not the future: cancellation cannot erase an
            // uncertain external effect or allow the account to be released.
            state.pending_call = true;
            submitted = true;
            let mut result = self
                .rpc(
                    index,
                    "tools/call",
                    json!({"name":tool,"arguments":arguments.clone()}),
                )
                .await?;
            validate_result(&result)?;
            if server == crate::chrome_connector::SERVER_NAME
                && crate::chrome_connector::policy_denied(&result)
            {
                self.policy_denied = true;
            }
            if server == crate::chrome_connector::SERVER_NAME && !self.policy_denied {
                if tool == "tabs_create_mcp" {
                    if let Some(tab) = crate::chrome_connector::created_tab(&result)? {
                        self.chrome_tabs.insert(tab);
                        self.store.begin_tool(
                            &self.run,
                            &format!("capability.claude_browser.tab.{tab}"),
                            "tabs_close_mcp",
                            &crate::digest(format!("tabId:{tab}")),
                        )?;
                    } else if crate::chrome_connector::group_missing(&result) {
                        result=json!({"isError":true,"content":[{"type":"text","text":"The persistent browser group is missing. Run xcb tools setup-browser to reconnect, then retry this task."}]});
                    }
                } else if tool == "tabs_close_mcp"
                    && let Some(tab) = arguments["tabId"]
                        .as_u64()
                        .filter(|tab| self.chrome_tabs.contains(tab))
                    && crate::chrome_connector::closed_tab(&result, tab)
                {
                    self.store.settle_tool(
                        &self.run,
                        &format!("capability.claude_browser.tab.{tab}"),
                    )?;
                    self.chrome_tabs.remove(&tab);
                }
            }
            self.servers[index]
                .as_mut()
                .expect("initialized server")
                .pending_call = false;
            self.effects = EffectState::Settled;
            Ok(result)
        }
        .await;
        let effects = if submitted {
            if result.is_ok() {
                EffectState::Settled
            } else {
                EffectState::Uncertain
            }
        } else {
            EffectState::None
        };
        CallOutcome { result, effects }
    }

    /// Available only to explicit CLI connection setup, never a provider tool.
    /// The persistent group/initial blank tab are deliberately not claimed as
    /// task-owned tabs. Task runs can create and settle their own tabs later.
    pub(crate) async fn bootstrap_chrome(&mut self) -> Result<()> {
        if self.config.servers.len() != 1
            || self.config.servers[0].name != crate::chrome_connector::SERVER_NAME
            || self.config.servers[0].credential_account.as_ref() != Some(&self.run.account)
            || self.servers[0].is_some()
        {
            return Err(Error::Protocol(
                "browser setup requires its own selected account probe",
            ));
        }
        self.browser_bootstrap = true;
        self.ensure_server(0).await?;
        let receipt = "xcb_persistent_browser_setup";
        self.store.begin_tool(
            &self.run,
            receipt,
            "tabs_context_mcp",
            &crate::digest("preserve existing group or create persistent initial blank tab"),
        )?;
        self.servers[0].as_mut().expect("initialized").pending_call = true;
        let result = self
            .rpc(
                0,
                "tools/call",
                json!({"name":"tabs_context_mcp","arguments":{"createIfEmpty":true}}),
            )
            .await?;
        validate_result(&result)?;
        let succeeded = crate::chrome_connector::setup_succeeded(&result);
        if succeeded || crate::chrome_connector::setup_was_refused(&result) {
            self.store.settle_tool(&self.run, receipt)?;
            self.servers[0].as_mut().expect("initialized").pending_call = false;
        }
        if succeeded {
            Ok(())
        } else {
            Err(Error::Unavailable(
                "browser setup did not connect; open Chrome with the Claude extension signed in to the selected account and retry",
            ))
        }
    }

    pub(crate) async fn finish_chrome_setup(&mut self) -> Result<()> {
        if !self.shutdown().await {
            return Err(Error::Unavailable(
                "browser setup has an unsettled operation; recover the selected account before retrying",
            ));
        }
        self.store
            .settle(&self.run, xcb_core::session::State::Idle, crate::now_ms())
    }

    /// Stop owned server processes only. A connector which attaches to the
    /// user's browser owns that connection, never the user's browser lifetime.
    pub async fn shutdown(&mut self) -> bool {
        if self.stopped {
            return self.servers.iter().all(Option::is_none)
                && self.credential_runs.iter().all(Option::is_none)
                && self.chrome_tabs.is_empty();
        }
        let mut all_joined = true;
        if let Some(index) = self
            .config
            .servers
            .iter()
            .position(|server| server.name == crate::chrome_connector::SERVER_NAME)
        {
            for tab in self.chrome_tabs.clone() {
                let Some(server) = self.servers[index].as_mut() else {
                    all_joined = false;
                    break;
                };
                if server.pending_call {
                    all_joined = false;
                    break;
                }
                server.pending_call = true;
                let result = self
                    .rpc(
                        index,
                        "tools/call",
                        json!({"name":"tabs_close_mcp","arguments":{"tabId":tab}}),
                    )
                    .await;
                let closed = result.as_ref().is_ok_and(|result| {
                    validate_result(result).is_ok()
                        && crate::chrome_connector::closed_tab(result, tab)
                }) && self
                    .store
                    .settle_tool(&self.run, &format!("capability.claude_browser.tab.{tab}"))
                    .is_ok();
                self.servers[index]
                    .as_mut()
                    .expect("started server")
                    .pending_call = !closed;
                all_joined &= closed;
                if closed {
                    self.chrome_tabs.remove(&tab);
                } else {
                    break;
                }
            }
        }
        for index in 0..self.servers.len() {
            let Some(tool) = self.config.servers[index].shutdown_tool.clone() else {
                continue;
            };
            let Some(server) = self.servers[index].as_mut() else {
                continue;
            };
            if server.pending_call || !server.native_pending.is_empty() || server.tools.is_none() {
                all_joined = false;
                server.pending_call = true;
                continue;
            }
            #[allow(unused_mut)]
            let mut arguments = json!({});
            #[cfg(unix)]
            if self.config.servers[index].transport == CapabilityTransport::CodexNative
                && tool == "turn_ended"
            {
                if !server.native_activity {
                    continue;
                }
                let Some(context) = &server.native_turn_context else {
                    all_joined = false;
                    server.pending_call = true;
                    continue;
                };
                arguments = context.clone();
            }
            server.pending_call = true;
            let receipt = format!("capability.{}.shutdown", self.config.servers[index].name);
            if self
                .store
                .begin_tool(
                    &self.run,
                    &receipt,
                    &tool,
                    &crate::digest(serde_json::to_vec(&arguments).expect("JSON arguments")),
                )
                .is_err()
            {
                all_joined = false;
                continue;
            }
            let result = self
                .rpc(
                    index,
                    "tools/call",
                    json!({"name":tool,"arguments":arguments}),
                )
                .await;
            let settled = result.as_ref().is_ok_and(|result| {
                validate_result(result).is_ok()
                    && result.get("isError").and_then(Value::as_bool) != Some(true)
            }) && self.store.settle_tool(&self.run, &receipt).is_ok();
            self.servers[index]
                .as_mut()
                .expect("started server")
                .pending_call = !settled;
            all_joined &= settled;
        }
        self.stopped = true;
        for (index, state) in self.servers.iter_mut().enumerate() {
            let Some(server) = state.as_mut() else {
                continue;
            };
            let joined = server.process.join_graceful(Duration::from_secs(2)).await;
            let settled = !server.pending_call && server.native_pending.is_empty();
            let mut cleared = joined
                && settled
                && self
                    .store
                    .clear_capability_custody(&self.run, &self.config.servers[index].name)
                    .is_ok();
            if cleared && let Some(probe) = &self.credential_runs[index] {
                cleared = self
                    .store
                    .clear_capability_custody(probe, &self.config.servers[index].name)
                    .and_then(|_| {
                        self.store
                            .settle(probe, xcb_core::session::State::Idle, crate::now_ms())
                    })
                    .is_ok();
                if cleared {
                    self.credential_runs[index] = None;
                }
            }
            server.artifacts.release_after_join(cleared, self.effects);
            all_joined &= cleared;
            if cleared {
                *state = None;
            }
        }
        all_joined
            && self.credential_runs.iter().all(Option::is_none)
            && self.chrome_tabs.is_empty()
    }
}

fn require_review(params: &mut Value) -> Result<()> {
    let params = params
        .as_object_mut()
        .ok_or(Error::Protocol("host tool parameters"))?;
    let metadata = params
        .entry("_meta")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or(Error::Protocol("host tool metadata"))?;
    let turn = metadata
        .entry("x-codex-turn-metadata")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or(Error::Protocol("host tool turn metadata"))?;
    turn.insert("node_repl_auto_review_required".into(), Value::Bool(true));
    Ok(())
}

fn points_into_workspace(value: &str, workspace: &std::path::Path) -> bool {
    let value = if value.starts_with('-') {
        value.split_once('=').map_or(value, |(_, value)| value)
    } else {
        value
    };
    let path = std::path::Path::new(value);
    path.is_absolute()
        && (path.starts_with(workspace)
            || path
                .ancestors()
                .find_map(|ancestor| xcb_core::canonical(ancestor).ok())
                .is_some_and(|path| path.starts_with(workspace)))
}

fn reject_workspace_paths(config: &CapabilityServer, workspace: &std::path::Path) -> Result<()> {
    if config.executable.starts_with(workspace)
        || xcb_core::canonical(&config.executable).is_ok_and(|path| path.starts_with(workspace))
        || config
            .args
            .iter()
            .any(|arg| points_into_workspace(arg, workspace))
        || config
            .environment
            .values()
            .any(|value| points_into_workspace(value, workspace))
        || config.bundles.iter().any(|bundle| {
            bundle.root.starts_with(workspace)
                || xcb_core::canonical(&bundle.root).is_ok_and(|path| path.starts_with(workspace))
        })
    {
        return Err(Error::Unavailable(
            "host tool configuration cannot load from the task workspace",
        ));
    }
    Ok(())
}

fn validate_inventory(result: Value) -> Result<Vec<Value>> {
    if result
        .get("nextCursor")
        .is_some_and(|cursor| !cursor.is_null())
    {
        return Err(Error::Protocol(
            "host tool inventory pagination is not supported",
        ));
    }
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .ok_or(Error::Protocol("host tool inventory"))?;
    if tools.len() > MAX_TOOLS || serde_json::to_vec(&result)?.len() > MAX_INVENTORY_BYTES {
        return Err(xcb_core::Error::Limit("host tool inventory").into());
    }
    let mut names = BTreeSet::new();
    let mut normalized = Vec::new();
    for tool in tools {
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| name_valid(name, 128))
            .ok_or(Error::Protocol("host tool name"))?;
        let schema = tool
            .get("inputSchema")
            .filter(|schema| {
                schema.is_object() && schema.get("type").and_then(Value::as_str) == Some("object")
            })
            .ok_or(Error::Protocol("host tool schema"))?;
        let description = tool
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !names.insert(name) || description.len() > 16 * 1024 {
            return Err(Error::Protocol("host tool inventory identity"));
        }
        normalized.push(json!({"name":name,"description":description,"inputSchema":schema}));
    }
    Ok(normalized)
}

fn validate_result(result: &Value) -> Result<()> {
    let content = result
        .get("content")
        .and_then(Value::as_array)
        .ok_or(Error::Protocol("host tool content"))?;
    if content.len() > 64 || serde_json::to_vec(result)?.len() > MAX_FRAME_BYTES {
        return Err(xcb_core::Error::Limit("host tool content").into());
    }
    if result
        .get("isError")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(Error::Protocol("host tool error flag"));
    }
    for item in content {
        match item.get("type").and_then(Value::as_str) {
            Some("text") if item.get("text").is_some_and(Value::is_string) => (),
            Some("image")
                if item.get("data").is_some_and(Value::is_string)
                    && matches!(
                        item.get("mimeType").and_then(Value::as_str),
                        Some("image/png" | "image/jpeg" | "image/webp")
                    ) => {}
            _ => return Err(Error::Protocol("unsupported host tool content")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> CapabilityServer {
        CapabilityServer {
            credential_account: None,
            name: "browser".into(),
            transport: CapabilityTransport::Shared,
            executable: PathBuf::from(if cfg!(windows) {
                "C:\\tools\\bridge.exe"
            } else {
                "/opt/tools/bridge"
            }),
            sha256: "a".repeat(64),
            bundles: vec![],
            args: vec![],
            env: vec![],
            environment: BTreeMap::new(),
            tools: None,
            shutdown_tool: None,
            features: vec![CapabilityFeature::Browser],
            timeout_ms: default_timeout(),
        }
    }

    #[test]
    fn owner_configuration_is_closed_bounded_and_unique() {
        config().validate().unwrap();
        let mut bad = config();
        bad.executable = PathBuf::from("bridge");
        assert!(bad.validate().is_err());
        let mut bad = config();
        bad.env = vec!["KEY=value".into()];
        assert!(bad.validate().is_err());
        let mut bad = config();
        bad.env = vec!["HOME".into()];
        assert!(bad.validate().is_err());
        let mut bad = config();
        bad.environment
            .insert("xdg_config_home".into(), "/owner".into());
        assert!(bad.validate().is_err());
        let mut bad = config();
        bad.sha256 = "a".repeat(63);
        assert!(bad.validate().is_err());
        assert!(
            CapabilityConfig {
                servers: vec![config(), config()]
            }
            .validate()
            .is_err()
        );
        assert!(
            serde_json::from_value::<CapabilityConfig>(json!({"servers":[],"untrusted":true}))
                .is_err()
        );
        let legacy: crate::config::Config = serde_json::from_value(json!({})).unwrap();
        assert!(legacy.capabilities.servers.is_empty());
    }

    #[test]
    fn tool_inventory_is_named_and_does_not_follow_pagination() {
        let tool = json!({"name":"capture","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}});
        let tools = validate_inventory(json!({"tools":[tool.clone()]})).unwrap();
        assert!(tools[0].get("annotations").is_none());
        assert!(validate_inventory(json!({"tools":[tool.clone(),tool.clone()]})).is_err());
        assert!(validate_inventory(json!({"tools":[tool],"nextCursor":"more"})).is_err());
        assert!(validate_inventory(json!({"tools":[{"name":"bad","inputSchema":{}}]})).is_err());
    }

    #[test]
    fn image_results_remain_images_and_resource_reads_are_not_implicit() {
        validate_result(&json!({"content":[{"type":"text","text":"page"},{"type":"image","mimeType":"image/png","data":"bounded; validated by shared image loader"}]})).unwrap();
        assert!(
            validate_result(
                &json!({"content":[{"type":"resource_link","uri":"file:///private/token"}]})
            )
            .is_err()
        );
        assert!(validate_result(&json!({"content":[],"isError":"false"})).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn executable_script_and_config_paths_cannot_resolve_into_consumer_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let workspace = private::directory(&base.join("workspace")).unwrap();
        let script = workspace.join("bridge.js");
        std::fs::write(&script, b"fixture").unwrap();
        let alias = base.join("bridge-alias.js");
        std::os::unix::fs::symlink(&script, &alias).unwrap();
        let mut server = config();
        server.args.push(alias.to_str().unwrap().to_owned());
        assert!(reject_workspace_paths(&server, &workspace).is_err());
        server.args.clear();
        server
            .environment
            .insert("BRIDGE_CONFIG".into(), script.to_str().unwrap().to_owned());
        assert!(reject_workspace_paths(&server, &workspace).is_err());
        server.environment.clear();
        server.args.push(format!("--config={}", script.display()));
        assert!(reject_workspace_paths(&server, &workspace).is_err());
        server.args = vec![workspace.join("a=b.js").to_str().unwrap().into()];
        assert!(reject_workspace_paths(&server, &workspace).is_err());
        server.args = vec![format!(
            "{}/../workspace/new.js",
            base.join("outside").display()
        )];
        std::fs::create_dir(base.join("outside")).unwrap();
        assert!(reject_workspace_paths(&server, &workspace).is_err());
        server.args = vec!["/opt/owner/bridge.js".into()];
        reject_workspace_paths(&server, &workspace).unwrap();
    }

    #[cfg(unix)]
    const SERVER_SCRIPT: &str = r#"
while IFS= read -r frame; do
    id="${frame#*\"id\":}"
    id="${id%%,*}"
    case "$frame" in
        *'"method":"initialize"'*)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}}}}\n' "$id" ;;
        *'"method":"tools/list"'*)
            printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"capture","inputSchema":{"type":"object"}},{"name":"fail","inputSchema":{"type":"object"}},{"name":"hang","inputSchema":{"type":"object"}},{"name":"reverse","inputSchema":{"type":"object"}},{"name":"shutdown","inputSchema":{"type":"object"}},{"name":"forbidden","inputSchema":{"type":"object"}},{"name":"tabs_create_mcp","inputSchema":{"type":"object"}},{"name":"tabs_close_mcp","inputSchema":{"type":"object"}},{"name":"tabs_context_mcp","inputSchema":{"type":"object"}},{"name":"computer","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
        *'"method":"tools/call"'*)
            case "$frame" in
                *'"node_repl_auto_review_required":true'*) ;;
                *) printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32000,"message":"review required"}}\n' "$id"; continue ;;
            esac
            case "$frame" in
                *'"name":"tabs_create_mcp"'*)
                    test "$1" = --claude-in-chrome-mcp && test "$CLAUDE_CODE_OAUTH_TOKEN" = sk-ant-oat01-synthetic_browser_fixture_not_real || exit 1
                    printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"Created new tab. Tab ID: 123"}]}}\n' "$id" ;;
                *'"name":"tabs_close_mcp"'*)
                    case "$frame" in *'"tabId":123'*) ;; *) exit 1 ;; esac
                    printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"Closed tab 123. 1 tab(s) remain."}]}}\n' "$id" ;;
                *'"name":"tabs_context_mcp"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"User-owned tab ID: 456"}]}}\n' "$id" ;;
                *'"name":"computer"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"isError":true,"content":[{"type":"text","text":"Permission denied by user"}]}}\n' "$id" ;;
                *'"name":"hang"'*) IFS= read -r unused ;;
                *'"name":"reverse"'*) printf '{"jsonrpc":"2.0","id":900,"method":"elicitation/create","params":{"message":"Approve"}}\n' ;;
                *'"name":"fail"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"known operation failed"}],"isError":true}}\n' "$id" ;;
                *'"name":"shutdown"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[]}}\n' "$id" ;;
                *) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"page"},{"type":"image","mimeType":"image/png","data":"fixture"}]}}\n' "$id" ;;
            esac ;;
    esac
done
"#;

    #[cfg(unix)]
    fn manager() -> (tempfile::TempDir, CapabilityManager) {
        manager_with_session(false)
    }

    #[cfg(unix)]
    fn manager_with_session(with_session: bool) -> (tempfile::TempDir, CapabilityManager) {
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let workspace = private::directory(&base.join("workspace")).unwrap();
        let store = Arc::new(Store::open(&base.join("state")).unwrap());
        let account = store
            .add_account(xcb_core::Provider::Codex, "fixture", 1, None)
            .unwrap();
        let run = if with_session {
            let model = xcb_core::models::ModelChoice {
                provider: xcb_core::Provider::Codex,
                id: xcb_core::Id::new("gpt-6.1-sol").unwrap(),
                label: "Sol".into(),
                mode: xcb_core::models::Mode::Fixed,
                resolved: None,
                effort: None,
                observed_at_ms: 1,
            };
            let session = store
                .create_session(&account.id, model, &workspace, 2)
                .unwrap();
            store.prepare_run(&session.id, session.revision, 3).unwrap()
        } else {
            store.prepare_probe(&account.id, None, 2).unwrap()
        };
        let run = store.mark_spawned(&run, i32::MAX as u32).unwrap();
        // macOS platform binaries cannot be executed after copying them away
        // from their system location. Pin an owner script, as real registered
        // script entrypoints are pinned, with the system interpreter in place.
        let executable = base.join("fixture-server");
        std::fs::write(&executable, format!("#!/bin/sh\n{SERVER_SCRIPT}")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let server = CapabilityServer {
            executable: executable.clone(),
            sha256: crate::process::executable_digest(&executable).unwrap(),
            args: vec![],
            tools: Some(vec![
                "capture".into(),
                "fail".into(),
                "hang".into(),
                "reverse".into(),
                "shutdown".into(),
            ]),
            shutdown_tool: Some("shutdown".into()),
            ..config()
        };
        (
            directory,
            CapabilityManager::new(
                CapabilityConfig {
                    servers: vec![server],
                },
                store,
                run,
                workspace,
            )
            .unwrap(),
        )
    }

    #[cfg(unix)]
    fn chrome_manager() -> (tempfile::TempDir, CapabilityManager) {
        let (directory, mut manager) = manager();
        let account = manager
            .store
            .add_account(xcb_core::Provider::Claude, "browser", 4, None)
            .unwrap();
        crate::auth::store_token(
            &manager.store,
            &account.id,
            b"sk-ant-oat01-synthetic_browser_fixture_not_real",
        )
        .unwrap();
        let fixture = &manager.config.servers[0];
        let (_, host_sha256) = crate::process::host_identity().unwrap();
        let mut pin = crate::process::Pin {
            provider: xcb_core::Provider::Claude,
            version: crate::chrome_connector::ADMITTED_VERSION.into(),
            executable: fixture.executable.clone(),
            sha256: fixture.sha256.clone(),
            host_sha256,
            observed_at_ms: 5,
        };
        pin.save(manager.store.root()).unwrap();
        let mut server = crate::chrome_connector::registration(&pin, &manager.workspace).unwrap();
        server.credential_account = Some(account.id);
        server.tools = Some(
            [
                "tabs_create_mcp",
                "tabs_close_mcp",
                "tabs_context_mcp",
                "computer",
            ]
            .map(str::to_string)
            .to_vec(),
        );
        manager.config.servers[0] = server;
        (directory, manager)
    }

    #[cfg(unix)]
    fn test_has_open_tools(store: &Store, run: &RunRecord) -> bool {
        let db = rusqlite::Connection::open_with_flags(
            store.root().join("xcb.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM tool_effects WHERE run=?1 AND settled=0)",
            [run.id.as_str()],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[cfg(unix)]
    fn test_capability_custody_present(store: &Store, run: &RunRecord) -> bool {
        !store
            .run(&run.id)
            .unwrap()
            .unwrap()
            .capability_processes
            .is_empty()
    }

    #[cfg(unix)]
    fn chrome_setup_manager(context: Value) -> (tempfile::TempDir, CapabilityManager) {
        let (directory, mut manager) = chrome_manager();
        let mut pin =
            crate::process::Pin::load(manager.store.root(), xcb_core::Provider::Claude).unwrap();
        let server = &mut manager.config.servers[0];
        let text = serde_json::to_string(&serde_json::to_string(&context).unwrap())
            .unwrap()
            .replace('\\', "\\\\");
        std::fs::write(
            &server.executable,
            format!(
                "#!/bin/sh\n{}",
                SERVER_SCRIPT.replace("\"User-owned tab ID: 456\"", &text)
            ),
        )
        .unwrap();
        server.sha256 = crate::process::executable_digest(&server.executable).unwrap();
        pin.sha256 = server.sha256.clone();
        pin.save(manager.store.root()).unwrap();
        manager
            .store
            .settle(
                &manager.run,
                xcb_core::session::State::Idle,
                crate::now_ms(),
            )
            .unwrap();
        manager.run = manager
            .store
            .prepare_probe(
                server.credential_account.as_ref().unwrap(),
                None,
                crate::now_ms(),
            )
            .unwrap();
        (directory, manager)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn chrome_setup_preserves_borrowed_group_and_persistent_initial_blank_tab() {
        for tab in [
            json!({"tabId":456,"title":"private borrowed title","url":"https://example.invalid/private"}),
            json!({"tabId":123,"title":"New Tab","url":"chrome://newtab/"}),
        ] {
            let (_directory, mut manager) =
                chrome_setup_manager(json!({"tabGroupId":42,"availableTabs":[tab]}));
            manager.bootstrap_chrome().await.unwrap();
            assert_eq!(manager.run.phase, "running");
            assert!(manager.run.pid.is_some());
            assert!(
                manager.chrome_tabs.is_empty(),
                "persistent setup does not claim any context tab"
            );
            assert!(manager.credential_runs[0].is_none());
            assert!(!test_has_open_tools(&manager.store, &manager.run));
            manager.finish_chrome_setup().await.unwrap();
            assert!(manager.store.unsettled_runs().unwrap().is_empty());
            assert!(!test_capability_custody_present(
                &manager.store,
                &manager.run
            ));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn chrome_setup_unclear_creation_retains_durable_receipt_and_account_custody() {
        let (_directory, mut manager) = chrome_setup_manager(json!({"availableTabs":[]}));
        assert!(manager.bootstrap_chrome().await.is_err());
        assert!(manager.finish_chrome_setup().await.is_err());
        assert!(test_has_open_tools(&manager.store, &manager.run));
        assert!(test_capability_custody_present(
            &manager.store,
            &manager.run
        ));
        assert_eq!(manager.store.unsettled_runs().unwrap().len(), 1);
        assert!(
            manager
                .store
                .prepare_probe(&manager.run.account, None, crate::now_ms())
                .is_err()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn chrome_setup_cancellation_joins_child_and_keeps_uncertain_creation_held() {
        let (_directory, manager) =
            chrome_setup_manager(json!({"tabGroupId":42,"availableTabs":[{"tabId":123}]}));
        manager
            .store
            .settle(
                &manager.run,
                xcb_core::session::State::Idle,
                crate::now_ms(),
            )
            .unwrap();
        let mut server = manager.config.servers[0].clone();
        let mut pin =
            crate::process::Pin::load(manager.store.root(), xcb_core::Provider::Claude).unwrap();
        let script = std::fs::read_to_string(&server.executable)
            .unwrap()
            .replace(
                "*'\"name\":\"tabs_context_mcp\"'*)",
                "*'\"name\":\"tabs_context_mcp\"'*) sleep 10;",
            );
        std::fs::write(&server.executable, script).unwrap();
        server.sha256 = crate::process::executable_digest(&server.executable).unwrap();
        pin.sha256 = server.sha256.clone();
        pin.save(manager.store.root()).unwrap();
        let store = manager.store.clone();
        let (stop, cancel) = tokio::sync::watch::channel(false);
        let cancellation = tokio::spawn(async move {
            for _ in 0..200 {
                if store
                    .unsettled_runs()
                    .unwrap()
                    .iter()
                    .any(|run| test_has_open_tools(&store, run))
                {
                    stop.send(true).unwrap();
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            panic!("setup did not submit its persistent group request");
        });
        assert!(
            crate::chrome_connector::setup(
                manager.store.root(),
                server,
                &manager.workspace,
                cancel
            )
            .await
            .is_err()
        );
        cancellation.await.unwrap();
        let runs = manager.store.unsettled_runs().unwrap();
        assert_eq!(runs.len(), 1);
        assert!(test_has_open_tools(&manager.store, &runs[0]));
        assert!(test_capability_custody_present(&manager.store, &runs[0]));
        assert_eq!(crate::os::process_exists(runs[0].pid.unwrap()), Some(false));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn chrome_setup_precancelled_does_not_start_child_or_hold_selected_account() {
        let (_directory, manager) = chrome_manager();
        let server = manager.config.servers[0].clone();
        let account = server.credential_account.clone().unwrap();
        let (_stop, cancel) = tokio::sync::watch::channel(true);
        assert!(
            crate::chrome_connector::setup(
                manager.store.root(),
                server,
                &manager.workspace,
                cancel
            )
            .await
            .is_err()
        );
        let runs = manager.store.unsettled_runs().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, manager.run.id);
        assert!(runs[0].capability_processes.is_empty());
        let probe = manager
            .store
            .prepare_probe(&account, None, crate::now_ms())
            .unwrap();
        manager
            .store
            .settle(&probe, xcb_core::session::State::Idle, crate::now_ms())
            .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn chrome_setup_missing_credential_releases_probe_without_starting_child() {
        let (_directory, manager) = chrome_manager();
        let server = manager.config.servers[0].clone();
        let account = server.credential_account.clone().unwrap();
        std::fs::remove_file(
            manager
                .store
                .account_root(&account)
                .unwrap()
                .join("subscription-token"),
        )
        .unwrap();
        let (_stop, cancel) = tokio::sync::watch::channel(false);
        assert!(
            crate::chrome_connector::setup(
                manager.store.root(),
                server,
                &manager.workspace,
                cancel
            )
            .await
            .is_err()
        );
        let runs = manager.store.unsettled_runs().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, manager.run.id);
        assert!(runs[0].capability_processes.is_empty());
        let probe = manager
            .store
            .prepare_probe(&account, None, crate::now_ms())
            .unwrap();
        manager
            .store
            .settle(&probe, xcb_core::session::State::Idle, crate::now_ms())
            .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn chrome_account_lease_and_only_explicit_created_tabs_are_released_after_cleanup() {
        let (_directory, mut manager) = chrome_manager();
        let name = crate::chrome_connector::SERVER_NAME;
        assert!(
            manager
                .call(name, "tabs_context_mcp", json!({"createIfEmpty":true}))
                .await
                .result
                .is_err()
        );
        assert!(manager.servers[0].is_none());
        manager
            .call(name, "tabs_context_mcp", json!({"createIfEmpty":false}))
            .await
            .result
            .unwrap();
        assert!(manager.chrome_tabs.is_empty());
        assert_eq!(manager.store.unsettled_runs().unwrap().len(), 2);
        let probe = manager.credential_runs[0].as_ref().unwrap();
        assert_eq!(probe.phase, "running");
        assert!(probe.pid.is_some());
        manager
            .call(name, "tabs_create_mcp", json!({}))
            .await
            .result
            .unwrap();
        assert_eq!(manager.chrome_tabs, BTreeSet::from([123]));
        assert!(manager.shutdown().await);
        assert!(manager.chrome_tabs.is_empty());
        assert_eq!(manager.store.unsettled_runs().unwrap().len(), 1);
        assert!(manager.credential_runs[0].is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn chrome_permission_denial_halts_cross_tool_retry_but_still_cleans_owned_tabs() {
        let (_directory, mut manager) = chrome_manager();
        let name = crate::chrome_connector::SERVER_NAME;
        manager
            .call(name, "tabs_create_mcp", json!({}))
            .await
            .result
            .unwrap();
        let result = manager
            .call(name, "computer", json!({"action":"screenshot","tabId":123}))
            .await
            .result
            .unwrap();
        assert_eq!(result["isError"], true);
        assert!(manager.policy_denied());
        assert!(
            manager
                .call(name, "tabs_context_mcp", json!({}))
                .await
                .result
                .is_err()
        );
        assert!(manager.shutdown().await);
        assert_eq!(manager.store.unsettled_runs().unwrap().len(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn chrome_credentials_never_reach_an_overridden_connector() {
        let (_directory, mut manager) = chrome_manager();
        manager.config.servers[0]
            .args
            .push("--permission-mode=bypassPermissions".into());
        assert!(
            manager
                .list(Some(crate::chrome_connector::SERVER_NAME))
                .await
                .is_err()
        );
        assert!(manager.servers[0].is_none());
        assert!(manager.credential_runs[0].is_none());
        assert_eq!(manager.store.unsettled_runs().unwrap().len(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn chrome_same_account_reuses_main_lease_without_releasing_it_on_bridge_shutdown() {
        let (_directory, mut manager) = chrome_manager();
        manager
            .store
            .settle(
                &manager.run,
                xcb_core::session::State::Idle,
                crate::now_ms(),
            )
            .unwrap();
        let account = manager.config.servers[0]
            .credential_account
            .as_ref()
            .unwrap();
        let run = manager
            .store
            .prepare_probe(account, None, crate::now_ms())
            .unwrap();
        manager.run = manager.store.mark_spawned(&run, i32::MAX as u32).unwrap();
        manager
            .call(
                crate::chrome_connector::SERVER_NAME,
                "tabs_create_mcp",
                json!({}),
            )
            .await
            .result
            .unwrap();
        assert!(manager.credential_runs[0].is_none());
        assert_eq!(manager.store.unsettled_runs().unwrap().len(), 1);
        assert!(manager.shutdown().await);
        assert_eq!(manager.store.unsettled_runs().unwrap().len(), 1);
    }

    #[test]
    fn required_review_keeps_native_thread_metadata_and_cannot_be_disabled() {
        let mut params = json!({"arguments":{},"_meta":{"owner":"test","x-codex-turn-metadata":{"thread_id":"example","node_repl_auto_review_required":false}}});
        require_review(&mut params).unwrap();
        assert_eq!(params["_meta"]["owner"], "test");
        assert_eq!(
            params["_meta"]["x-codex-turn-metadata"]["thread_id"],
            "example"
        );
        assert_eq!(
            params["_meta"]["x-codex-turn-metadata"]["node_repl_auto_review_required"],
            true
        );
        assert!(require_review(&mut json!({"_meta":false})).is_err());
    }

    #[cfg(unix)]
    async fn native_manager(with_session: bool) -> (tempfile::TempDir, CapabilityManager) {
        let (directory, mut manager) = manager_with_session(with_session);
        manager.config.servers[0].name = "cua_repl".into();
        manager.config.servers[0].transport = CapabilityTransport::CodexNative;
        assert_eq!(manager.list(None).await.unwrap()["servers"], json!([]));
        assert!(
            manager
                .call("cua_repl", "capture", json!({}))
                .await
                .result
                .is_err()
        );
        manager.native_open("cua_repl").await.unwrap();
        manager
            .native_send(
                "cua_repl",
                &json!({"jsonrpc":"2.0","id":101,"method":"initialize","params":{}}),
            )
            .await
            .unwrap();
        let response: Value =
            serde_json::from_slice(&manager.native_frame("cua_repl").await.unwrap().unwrap())
                .unwrap();
        assert_eq!(response["id"], 101);
        manager
            .native_send(
                "cua_repl",
                &json!({"jsonrpc":"2.0","id":102,"method":"tools/list","params":{}}),
            )
            .await
            .unwrap();
        let response: Value =
            serde_json::from_slice(&manager.native_frame("cua_repl").await.unwrap().unwrap())
                .unwrap();
        let inventory = manager
            .native_mark_initialized("cua_repl", response["result"].clone())
            .unwrap();
        assert!(!inventory.to_string().contains("forbidden"));
        (directory, manager)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_submission_requires_exact_durable_intent_and_settles_after_result() {
        let (_directory, mut manager) = native_manager(false).await;
        let frame = json!({"jsonrpc":"2.0","id":103,"method":"tools/call","params":{"name":"capture","arguments":{}}});
        assert!(manager.native_send("cua_repl", &frame).await.is_err());
        manager
            .native_effect_begin("cua_repl", "103", "capture", &json!({}))
            .unwrap();
        assert_eq!(manager.effects(), EffectState::Uncertain);
        assert!(
            manager
                .native_effect_begin("cua_repl", "103", "capture", &json!({}))
                .is_err()
        );
        assert!(
            manager
                .native_effect_settle("cua_repl", "103", &json!({"content":[]}))
                .is_err()
        );
        let mut changed = frame.clone();
        changed["params"]["arguments"] = json!({"changed":true});
        assert!(manager.native_send("cua_repl", &changed).await.is_err());
        manager.native_send("cua_repl", &frame).await.unwrap();
        assert!(manager.native_send("cua_repl", &frame).await.is_err());
        let response: Value =
            serde_json::from_slice(&manager.native_frame("cua_repl").await.unwrap().unwrap())
                .unwrap();
        manager
            .native_effect_settle("cua_repl", "103", &response["result"])
            .unwrap();
        assert_eq!(manager.effects(), EffectState::Settled);
        assert!(
            manager
                .native_effect_begin("cua_repl", "103", "capture", &json!({}))
                .is_err()
        );
        assert!(manager.shutdown().await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_images_and_computer_requirement_are_durable_before_fallback() {
        use base64::Engine;
        let (_directory, mut manager) = native_manager(true).await;
        let session = manager.run.session.clone().unwrap();
        assert!(
            !manager
                .store
                .session(&session)
                .unwrap()
                .unwrap()
                .requirements
                .signed_in_browser
        );
        manager.servers[0]
            .as_mut()
            .unwrap()
            .tools
            .as_mut()
            .unwrap()
            .push(json!({"name":"js","inputSchema":{"type":"object"}}));
        manager
            .native_effect_begin("cua_repl", "103", "js", &json!({}))
            .unwrap();
        assert!(
            manager
                .store
                .session(&session)
                .unwrap()
                .unwrap()
                .requirements
                .signed_in_browser
        );
        manager.native_send("cua_repl",&json!({"jsonrpc":"2.0","id":103,"method":"tools/call","params":{"name":"js","arguments":{}}})).await.unwrap();
        let _response = manager.native_frame("cua_repl").await.unwrap().unwrap();
        let attachment =
            crate::attachments::from_rgba(manager.store.root(), 2, 2, vec![255; 16]).unwrap();
        let data = base64::engine::general_purpose::STANDARD
            .encode(crate::attachments::read(manager.store.root(), &attachment).unwrap());
        let reply = manager.native_effect_settle("cua_repl","103",&json!({"content":[{"type":"text","text":"Page ready"},{"type":"image","mimeType":"image/png","data":data}]})).unwrap();
        assert_eq!(reply["content"][1]["data"], data);
        let messages = manager.store.messages(&session, 10).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].attachments, vec![attachment]);
        assert!(messages[0].text.contains("Page ready"));
        assert!(!messages[0].text.contains(&data));
        assert!(manager.shutdown().await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_lost_response_keeps_durable_receipt_and_custody_after_process_exit() {
        let (_directory, mut manager) = native_manager(false).await;
        manager
            .native_effect_begin("cua_repl", "103", "hang", &json!({}))
            .unwrap();
        manager.native_send("cua_repl",&json!({"jsonrpc":"2.0","id":103,"method":"tools/call","params":{"name":"hang","arguments":{}}})).await.unwrap();
        let pid = manager.servers[0].as_ref().unwrap().process.pid();
        assert!(!manager.shutdown().await);
        crate::process::prove_process_group_absent(pid).unwrap();
        assert_eq!(manager.effects(), EffectState::Uncertain);
        assert_eq!(
            manager
                .store
                .run(&manager.run.id)
                .unwrap()
                .unwrap()
                .capability_processes
                .len(),
            1
        );
        let receipt = format!("native.cua_repl.{}", &crate::digest("103")[..32]);
        assert!(
            manager
                .store
                .begin_tool(&manager.run, &receipt, "hang", &crate::digest("{}"))
                .is_err()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pinned_server_inventory_review_metadata_and_shutdown_are_checked() {
        let (_directory, mut manager) = manager();
        assert!(
            manager
                .store
                .run(&manager.run.id)
                .unwrap()
                .unwrap()
                .capability_processes
                .is_empty()
        );
        let inventory = manager.list(Some("browser")).await.unwrap();
        assert_eq!(
            inventory["servers"][0]["tools"].as_array().unwrap().len(),
            5
        );
        assert!(!inventory.to_string().contains("forbidden"));
        let denied = manager.call("browser", "forbidden", json!({})).await;
        assert!(denied.result.is_err());
        assert_eq!(denied.effects, EffectState::None);
        let result = manager.call("browser", "capture", json!({})).await;
        assert_eq!(result.effects, EffectState::Settled);
        assert_eq!(result.result.unwrap()["content"][1]["type"], "image");
        let failed = manager.call("browser", "fail", json!({})).await;
        assert_eq!(failed.effects, EffectState::Settled);
        assert_eq!(failed.result.unwrap()["isError"], true);
        assert!(manager.shutdown().await);
        assert!(manager.shutdown().await);
        assert!(
            manager
                .store
                .run(&manager.run.id)
                .unwrap()
                .unwrap()
                .capability_processes
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_calls_keep_effects_and_durable_custody_uncertain() {
        let (_directory, mut manager) = manager();
        manager.list(None).await.unwrap();
        let pid = manager.servers[0].as_ref().unwrap().process.pid();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(30),
                manager.call("browser", "hang", json!({}))
            )
            .await
            .is_err()
        );
        assert_eq!(manager.effects(), EffectState::Uncertain);
        assert!(!manager.shutdown().await);
        assert!(!manager.shutdown().await);
        crate::process::prove_process_group_absent(pid).unwrap();
        assert_eq!(
            manager
                .store
                .run(&manager.run.id)
                .unwrap()
                .unwrap()
                .capability_processes
                .len(),
            1
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reverse_approval_requests_are_refused_and_never_auto_accepted() {
        let (_directory, mut manager) = manager();
        let result = manager.call("browser", "reverse", json!({})).await;
        assert!(matches!(
            result.result,
            Err(Error::Protocol(
                "host tool server requested unsupported host operation"
            ))
        ));
        assert_eq!(result.effects, EffectState::Uncertain);
        assert!(!manager.shutdown().await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn replaced_executable_is_refused_before_process_or_custody_exists() {
        let (_directory, mut manager) = manager();
        manager.config.servers[0].sha256 = "0".repeat(64);
        assert!(manager.list(None).await.is_err());
        assert!(
            manager
                .store
                .run(&manager.run.id)
                .unwrap()
                .unwrap()
                .capability_processes
                .is_empty()
        );
        assert!(manager.shutdown().await);
    }
}
