//! A private MCP relay into the active Codex thread's automatic reviewer.
//!
//! The confined provider runs only the byte relay. The registered connector
//! stays host-owned, including its process custody and external-effect record.
//! Review requests travel unchanged back to Codex; this module never answers
//! them and always requests the connector's strict review path.

use crate::{
    Error, Result,
    capabilities::{CapabilityConfig, CapabilityManager, CapabilityServer},
    private,
    store::{RunRecord, Store},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::Duration,
};
use tokio::{io::BufReader, net::UnixListener, sync::watch, task::JoinHandle};
use xcb_core::policy::EffectState;

const FRAME_BYTES: usize = 16 * 1024 * 1024;
const TOTAL_BYTES: usize = 256 * 1024 * 1024;
const MAX_PENDING: usize = 32;

/// Fixed protocol milestones only. Never contains server output, arguments,
/// credential material, browser state, or the private relay configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeStartup {
    WaitingForClient,
    ClientAuthenticated,
    ConnectorStarted,
    Initialized,
    Ready,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeDiagnostic {
    pub startup: NativeStartup,
    pub failed: bool,
}

pub struct NativeInspection {
    pub diagnostic: NativeDiagnostic,
    pub initialization: Result<()>,
    pub unhandled_notice_sha256: Option<String>,
}

#[derive(Clone, Copy)]
pub struct NativeProxyReceipt {
    pub joined: bool,
    pub effects: EffectState,
    pub pending_attention: bool,
}

pub struct NativeMcpProxy {
    directory: PathBuf,
    socket: PathBuf,
    token: String,
    tools: Vec<String>,
    stop: watch::Sender<bool>,
    turn_context: watch::Sender<Option<(String, String)>>,
    task: Option<JoinHandle<NativeProxyReceipt>>,
    completion: Option<NativeProxyReceipt>,
    effect: Arc<AtomicU8>,
    attention: Arc<AtomicBool>,
    startup: Arc<AtomicU8>,
    failed: Arc<AtomicBool>,
}

impl NativeMcpProxy {
    pub async fn start(
        config: CapabilityServer,
        store: Arc<Store>,
        run: RunRecord,
        workspace: PathBuf,
    ) -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        config.validate()?;
        if config.name != "cua_repl"
            || config.transport != crate::capabilities::CapabilityTransport::CodexNative
            || config.shutdown_tool.as_deref() != Some("turn_ended")
        {
            return Err(Error::Unavailable(
                "native MCP review is limited to the computer-use connector",
            ));
        }
        let tools = config
            .tools
            .clone()
            .ok_or(Error::Protocol("native MCP inventory is not declared"))?;
        if !tools.iter().any(|name| name == "js")
            || !tools.iter().any(|name| name == "turn_ended")
            || tools
                .iter()
                .any(|name| !["js", "js_reset", "turn_ended"].contains(&name.as_str()))
        {
            return Err(Error::Protocol("unsupported native computer-use tool"));
        }
        let temporary_root = std::fs::canonicalize("/tmp")?;
        let directory = private::directory(
            &temporary_root.join(format!("xcb-cua-{}", uuid::Uuid::new_v4().simple())),
        )?;
        let socket = directory.join("mcp.sock");
        let listener = match UnixListener::bind(&socket) {
            Ok(listener) => listener,
            Err(error) => {
                let _ = std::fs::remove_dir(&directory);
                return Err(error.into());
            }
        };
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let expected = token.clone();
        let name = config.name.clone();
        let mut manager = CapabilityManager::new(
            CapabilityConfig {
                servers: vec![config],
            },
            store,
            run,
            workspace,
        )?;
        let (stop, mut stopped) = watch::channel(false);
        let (turn_context, context) = watch::channel::<Option<(String, String)>>(None);
        let effect = Arc::new(AtomicU8::new(0));
        let current = effect.clone();
        let attention = Arc::new(AtomicBool::new(false));
        let observed_attention = attention.clone();
        let startup = Arc::new(AtomicU8::new(0));
        let observed_startup = startup.clone();
        let failed = Arc::new(AtomicBool::new(false));
        let observed_failure = failed.clone();
        let task = tokio::spawn(async move {
            let serving = async {
                let (stream, _) = listener.accept().await?;
                drop(listener);
                let (read, write) = stream.into_split();
                let mut read = BufReader::new(read);
                let mut buffer = Vec::new();
                let hello = tokio::time::timeout(
                    Duration::from_secs(10),
                    read_frame(&mut read, &mut buffer),
                )
                .await
                .map_err(|_| Error::Protocol("native MCP authentication deadline"))??
                .ok_or(Error::Protocol("native MCP authentication absent"))?;
                let hello: Value = serde_json::from_slice(&hello)?;
                if hello.as_object().map(|value| value.len()) != Some(1)
                    || hello["token"].as_str() != Some(expected.as_str())
                {
                    return Err(Error::Protocol("native MCP authentication failed"));
                }
                observed_startup.store(1, Ordering::Release);
                manager.native_open(&name).await?;
                observed_startup.store(2, Ordering::Release);
                relay(
                    &mut manager,
                    &name,
                    read,
                    write,
                    &current,
                    &observed_attention,
                    &observed_startup,
                )
                .await
            };
            let failed = tokio::select! {
                _ = stopped.changed() => false,
                // Losing the client/server before the owner requests shutdown
                // is not a ready relay, including a clean but premature EOF.
                _result = serving => true,
            };
            observed_failure.store(failed, Ordering::Release);
            // A closed relay is never evidence that its connector has stopped.
            let context = context.borrow().clone();
            let context_set = match context {
                Some((session, turn)) => manager
                    .native_set_turn_context(&name, &session, &turn)
                    .is_ok(),
                None => true,
            };
            let joined = manager.shutdown().await && context_set;
            let effects = manager.effects();
            current.store(effect_code(effects), Ordering::Release);
            NativeProxyReceipt {
                joined,
                effects,
                pending_attention: observed_attention.load(Ordering::Acquire),
            }
        });
        Ok(Self {
            directory,
            socket,
            token,
            tools,
            stop,
            turn_context,
            task: Some(task),
            completion: None,
            effect,
            attention,
            startup,
            failed,
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    pub fn set_turn_context(&self, session: &str, turn: &str) -> Result<()> {
        if [session, turn].iter().any(|value| {
            value.is_empty() || value.len() > 160 || value.chars().any(char::is_control)
        }) {
            return Err(Error::Protocol("native MCP turn identity"));
        }
        let next = (session.to_owned(), turn.to_owned());
        if self
            .turn_context
            .borrow()
            .as_ref()
            .is_some_and(|previous| previous != &next)
        {
            return Err(Error::Protocol("native MCP turn identity changed"));
        }
        self.turn_context
            .send(Some(next))
            .map_err(|_| Error::Protocol("native MCP connector already stopped"))
    }

    /// Native app-server configuration only; never include this value in CLI
    /// diagnostics, transcripts, or model-visible tool discovery results.
    pub fn configuration(&self, helper: &Path) -> Result<Value> {
        if !helper.is_absolute() || helper.as_os_str().len() > 4096 {
            return Err(Error::PrivateState);
        }
        Ok(
            json!({"command":helper,"args":["native-mcp-stdio"],"enabled":true,
            "enabled_tools":self.tools,"env":{"XCB_MCP_SOCKET":self.socket,"XCB_MCP_TOKEN":self.token}}),
        )
    }

    pub fn effects(&self) -> EffectState {
        match self.effect.load(Ordering::Acquire) {
            0 => EffectState::None,
            1 => EffectState::Settled,
            _ => EffectState::Uncertain,
        }
    }

    pub fn pending_attention(&self) -> bool {
        self.attention.load(Ordering::Acquire)
    }

    pub fn diagnostic(&self) -> NativeDiagnostic {
        NativeDiagnostic {
            startup: match self.startup.load(Ordering::Acquire) {
                0 => NativeStartup::WaitingForClient,
                1 => NativeStartup::ClientAuthenticated,
                2 => NativeStartup::ConnectorStarted,
                3 => NativeStartup::Initialized,
                _ => NativeStartup::Ready,
            },
            failed: self.failed.load(Ordering::Acquire),
        }
    }

    pub async fn shutdown(&mut self) -> NativeProxyReceipt {
        let _ = self.stop.send(true);
        let Some(task) = self.task.as_mut() else {
            return self.completion.unwrap_or(NativeProxyReceipt {
                joined: false,
                effects: EffectState::Uncertain,
                pending_attention: self.pending_attention(),
            });
        };
        match tokio::time::timeout(Duration::from_secs(15), task).await {
            Ok(result) => {
                self.task = None;
                let receipt = result.unwrap_or(NativeProxyReceipt {
                    joined: false,
                    effects: EffectState::Uncertain,
                    pending_attention: self.pending_attention(),
                });
                self.effect
                    .store(effect_code(receipt.effects), Ordering::Release);
                self.completion = Some(receipt);
                if receipt.joined {
                    self.remove_socket();
                }
                receipt
            }
            Err(_) => NativeProxyReceipt {
                joined: false,
                effects: EffectState::Uncertain,
                pending_attention: self.pending_attention(),
            },
        }
    }

    fn remove_socket(&self) {
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_dir(&self.directory);
    }
}

impl Drop for NativeMcpProxy {
    fn drop(&mut self) {
        // Let the owner task join its connector. Dropping a JoinHandle detaches
        // it; aborting it here would discard the last chance to prove cleanup.
        let _ = self.stop.send(true);
        if self.completion.is_some_and(|receipt| receipt.joined) {
            self.remove_socket();
        }
    }
}

fn effect_code(effect: EffectState) -> u8 {
    match effect {
        EffectState::None => 0,
        EffectState::Settled => 1,
        EffectState::Uncertain => 2,
    }
}

async fn read_frame<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
) -> Result<Option<Vec<u8>>> {
    crate::wire_helpers::frame(
        reader,
        buffer,
        FRAME_BYTES,
        "native MCP frame bound",
        "incomplete native MCP frame",
    )
    .await
}

fn rpc_id(value: &Value) -> Result<String> {
    if value
        .as_str()
        .is_some_and(|text| text.is_empty() || text.len() > 160)
        || !(value.is_string() || value.as_i64().is_some() || value.as_u64().is_some())
    {
        return Err(Error::Protocol("native MCP request identity"));
    }
    Ok(serde_json::to_string(value)?)
}

fn require_review(frame: &mut Value) -> Result<()> {
    let params = frame
        .get_mut("params")
        .and_then(Value::as_object_mut)
        .ok_or(Error::Protocol("native MCP tool parameters"))?;
    let meta = params
        .entry("_meta")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or(Error::Protocol("native MCP tool metadata"))?;
    let turn = meta
        .entry("x-codex-turn-metadata")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or(Error::Protocol("native MCP turn metadata"))?;
    turn.insert("node_repl_auto_review_required".into(), Value::Bool(true));
    Ok(())
}

struct Pending {
    method: String,
    effect: bool,
}

fn explicit_review_failure(result: &Value) -> bool {
    result["isError"] == true
        && result["content"].as_array().is_some_and(|content| {
            content.len() == 1
                && content[0]["type"] == "text"
                && content[0]["text"] == "Automated review of this operation failed. Do not proceed without asking the user for explicit approval."
        })
}

async fn relay(
    manager: &mut CapabilityManager,
    name: &str,
    mut reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    mut writer: tokio::net::unix::OwnedWriteHalf,
    effect: &AtomicU8,
    attention: &AtomicBool,
    startup: &AtomicU8,
) -> Result<()> {
    let mut requests = BTreeMap::<String, Pending>::new();
    let mut reverse = BTreeSet::new();
    let mut tools = BTreeSet::new();
    let mut initialized = false;
    let mut initialize_sent = false;
    let mut total = 0usize;
    let mut buffer = Vec::new();
    for _ in 0..16384 {
        let (from_client, bytes) = tokio::select! {
            frame = read_frame(&mut reader, &mut buffer) => (true, frame?),
            frame = manager.native_frame(name) => (false, frame?),
        };
        let Some(bytes) = bytes else {
            return Ok(());
        };
        total = total
            .checked_add(bytes.len())
            .filter(|bytes| *bytes <= TOTAL_BYTES)
            .ok_or(Error::Protocol("native MCP total bytes"))?;
        let mut frame: Value = serde_json::from_slice(&bytes)?;
        if frame["jsonrpc"] != "2.0" || !frame.is_object() {
            return Err(Error::Protocol("native MCP envelope"));
        }
        if let Some(method) = frame
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            if frame.get("result").is_some() || frame.get("error").is_some() {
                return Err(Error::Protocol("native MCP mixed envelope"));
            }
            if from_client {
                if let Some(id) = frame.get("id") {
                    let id = rpc_id(id)?;
                    if requests.len() >= MAX_PENDING || requests.contains_key(&id) {
                        return Err(Error::Protocol("native MCP outstanding request"));
                    }
                    let is_effect = match method.as_str() {
                        "initialize" if !initialize_sent => {
                            initialize_sent = true;
                            false
                        }
                        "tools/list" | "ping" if initialized => false,
                        "tools/call" if initialized && !tools.is_empty() => {
                            if attention.load(Ordering::Acquire) {
                                return Err(Error::Unavailable(
                                    "computer-use approval requires user attention",
                                ));
                            }
                            let tool = frame
                                .pointer("/params/name")
                                .and_then(Value::as_str)
                                .ok_or(Error::Protocol("native MCP tool name"))?;
                            if !tools.contains(tool)
                                || !frame
                                    .pointer("/params/arguments")
                                    .is_some_and(Value::is_object)
                            {
                                return Err(Error::Protocol("native MCP tool not admitted"));
                            }
                            require_review(&mut frame)?;
                            let tool = frame["params"]["name"]
                                .as_str()
                                .ok_or(Error::Protocol("native MCP tool name"))?;
                            manager.native_effect_begin(
                                name,
                                &id,
                                tool,
                                &frame["params"]["arguments"],
                            )?;
                            effect.store(2, Ordering::Release);
                            true
                        }
                        _ => return Err(Error::Protocol("native MCP client method not admitted")),
                    };
                    requests.insert(
                        id,
                        Pending {
                            method,
                            effect: is_effect,
                        },
                    );
                } else if method != "notifications/initialized"
                    && method != "notifications/cancelled"
                {
                    return Err(Error::Protocol(
                        "native MCP client notification not admitted",
                    ));
                }
            } else if let Some(id) = frame.get("id") {
                if method != "elicitation/create"
                    || !requests.values().any(|pending| pending.effect)
                    || reverse.len() >= MAX_PENDING
                    || !reverse.insert(rpc_id(id)?)
                {
                    return Err(Error::Protocol("native MCP reverse request not admitted"));
                }
            } else if method == "notifications/tools/list_changed" {
                return Err(Error::Protocol("native MCP inventory changed during run"));
            }
        } else {
            let id = rpc_id(&frame["id"])?;
            if frame.get("result").is_some() == frame.get("error").is_some() {
                return Err(Error::Protocol("native MCP response envelope"));
            }
            if from_client {
                if !reverse.remove(&id) {
                    return Err(Error::Protocol("native MCP unexpected review response"));
                }
                if matches!(
                    frame["result"]["action"].as_str(),
                    Some("decline" | "cancel")
                ) {
                    attention.store(true, Ordering::Release);
                }
            } else {
                let pending = requests
                    .remove(&id)
                    .ok_or(Error::Protocol("native MCP response identity"))?;
                match pending.method.as_str() {
                    "initialize" => {
                        let result = &frame["result"];
                        if !matches!(
                            result["protocolVersion"].as_str(),
                            Some("2024-11-05" | "2025-03-26" | "2025-06-18" | "2025-11-25")
                        ) || !result
                            .pointer("/capabilities/tools")
                            .is_some_and(Value::is_object)
                        {
                            return Err(Error::Protocol("native MCP initialization rejected"));
                        }
                        initialized = true;
                        startup.store(3, Ordering::Release);
                    }
                    "tools/list" => {
                        let result =
                            manager.native_mark_initialized(name, frame["result"].clone())?;
                        tools = result["tools"]
                            .as_array()
                            .ok_or(Error::Protocol("native MCP tools missing"))?
                            .iter()
                            .map(|tool| {
                                tool["name"]
                                    .as_str()
                                    .map(str::to_owned)
                                    .ok_or(Error::Protocol("native MCP tool name"))
                            })
                            .collect::<Result<_>>()?;
                        frame["result"] = result;
                        startup.store(4, Ordering::Release);
                    }
                    _ => {}
                }
                if pending.effect {
                    if explicit_review_failure(&frame["result"]) {
                        attention.store(true, Ordering::Release);
                    }
                    frame["result"] = manager.native_effect_settle(name, &id, &frame["result"])?;
                    effect.store(effect_code(manager.effects()), Ordering::Release);
                }
            }
        }
        if from_client {
            manager.native_send(name, &frame).await?;
        } else {
            crate::wire_helpers::write_frame(
                &mut writer,
                &frame,
                FRAME_BYTES,
                "native MCP output bound",
            )
            .await?;
        }
    }
    Err(Error::Protocol("native MCP frame count"))
}

/// Confined helper entry point. It has no connector access or approval logic;
/// it only copies bounded JSON frames into the parent run's authenticated pipe.
pub async fn run_native_mcp_stdio(path: &Path, token: &str) -> Result<()> {
    if !path.is_absolute() || !xcb_core::hex64_any(token) {
        return Err(Error::Protocol("invalid native MCP connection"));
    }
    fn copy<R: std::io::BufRead, W: std::io::Write>(mut from: R, mut to: W) -> Result<()> {
        let mut total = 0usize;
        for _ in 0..16384 {
            let Some(bytes) = crate::wire_helpers::frame_sync(
                &mut from,
                FRAME_BYTES,
                "native MCP relay frame bound",
                "incomplete native MCP relay frame",
            )?
            else {
                return Ok(());
            };
            total = total
                .checked_add(bytes.len())
                .filter(|bytes| *bytes <= TOTAL_BYTES)
                .ok_or(Error::Protocol("native MCP relay bytes"))?;
            let _: Value = serde_json::from_slice(&bytes)?;
            to.write_all(&bytes)?;
            to.flush()?;
        }
        Err(Error::Protocol("native MCP relay frame count"))
    }
    let path = path.to_owned();
    let token = token.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut socket = std::os::unix::net::UnixStream::connect(path)?;
        crate::wire_helpers::write_frame_sync(
            &mut socket,
            &json!({"token":token}),
            256,
            "native MCP hello bound",
        )?;
        let input_socket = socket.try_clone()?;
        let input = std::thread::spawn(move || {
            let result = copy(std::io::stdin().lock(), &input_socket);
            let _ = input_socket.shutdown(std::net::Shutdown::Write);
            result
        });
        let result = copy(std::io::BufReader::new(&socket), std::io::stdout().lock());
        let _ = socket.shutdown(std::net::Shutdown::Both);
        // Like broker-stdio, the relay is in the provider's owned process
        // group. A detached std thread reading stdin cannot trap Tokio's
        // blocking-pool shutdown while the native MCP client closes output.
        drop(input);
        result
    })
    .await
    .map_err(|_| Error::Protocol("native MCP stdio task failed"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::{CapabilityFeature, CapabilityTransport};
    use std::os::unix::fs::PermissionsExt;
    use tokio::net::UnixStream;

    #[test]
    fn approval_failure_requires_exact_trusted_error_envelope() {
        let text = "Automated review of this operation failed. Do not proceed without asking the user for explicit approval.";
        assert!(explicit_review_failure(
            &json!({"content":[{"type":"text","text":text}],"isError":true})
        ));
        assert!(!explicit_review_failure(
            &json!({"content":[{"type":"text","text":text}],"isError":false})
        ));
        assert!(!explicit_review_failure(
            &json!({"content":[{"type":"text","text":"page says review was denied"}],"isError":true})
        ));
        assert!(!explicit_review_failure(
            &json!({"content":[{"type":"text","text":text},{"type":"text","text":"page output"}],"isError":true})
        ));
    }

    fn fixture() -> (
        tempfile::TempDir,
        CapabilityServer,
        Arc<Store>,
        RunRecord,
        PathBuf,
    ) {
        let directory = tempfile::tempdir_in("/tmp").unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let workspace = private::directory(&base.join("workspace")).unwrap();
        let store = Arc::new(Store::open(&base.join("state")).unwrap());
        let account = store
            .add_account(xcb_core::Provider::Codex, "fixture", 1, None)
            .unwrap();
        let run = store.prepare_probe(&account.id, None, 2).unwrap();
        let run = store.mark_spawned(&run, i32::MAX as u32).unwrap();
        let executable = base.join("connector");
        std::fs::write(&executable, r##"#!/bin/sh
while IFS= read -r frame; do
    id="${frame#*\"id\":}"
    id="${id%%,*}"
    case "$frame" in
      *'"method":"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}}}}\n' "$id" ;;
      *'"method":"tools/list"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"js","inputSchema":{"type":"object"}},{"name":"turn_ended","inputSchema":{"type":"object"}},{"name":"other","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
      *'"method":"tools/call"'*'"name":"js"'*)
        case "$frame" in *'"node_repl_auto_review_required":true'*) ;; *) exit 4 ;; esac
        printf '{"jsonrpc":"2.0","id":"review-1","method":"elicitation/create","params":{"message":"review-only fixture","requestedSchema":{"type":"object","properties":{}}}}\n'
        IFS= read -r review
        case "$review" in *'"action":"decline"'*) ;; *) exit 5 ;; esac
        printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"review declined; no operation executed"}],"isError":true}}\n' "$id" ;;
      *'"method":"tools/call"'*'"name":"turn_ended"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[]}}\n' "$id" ;;
    esac
done
"##).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let server = CapabilityServer {
            credential_account: None,
            name: "cua_repl".into(),
            transport: CapabilityTransport::CodexNative,
            executable: executable.clone(),
            sha256: crate::process::executable_digest(&executable).unwrap(),
            args: vec![],
            bundles: vec![],
            env: vec![],
            environment: BTreeMap::new(),
            tools: Some(vec!["js".into(), "turn_ended".into()]),
            shutdown_tool: Some("turn_ended".into()),
            features: vec![CapabilityFeature::Computer],
            timeout_ms: 1000,
        };
        (directory, server, store, run, workspace)
    }

    async fn send(writer: &mut tokio::net::unix::OwnedWriteHalf, frame: Value) {
        crate::wire_helpers::write_frame(writer, &frame, FRAME_BYTES, "test frame")
            .await
            .unwrap();
    }
    async fn receive(reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>) -> Value {
        let bytes =
            tokio::time::timeout(Duration::from_secs(4), read_frame(reader, &mut Vec::new()))
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn native_review_round_trip_preserves_denial_and_joins_connector() {
        let (_directory, server, store, run, workspace) = fixture();
        let mut proxy = NativeMcpProxy::start(server, store.clone(), run.clone(), workspace)
            .await
            .unwrap();
        proxy
            .set_turn_context("fixture-thread", "fixture-turn")
            .unwrap();
        assert!(proxy.set_turn_context("changed", "fixture-turn").is_err());
        let stream = UnixStream::connect(proxy.socket_path()).await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        send(&mut writer, json!({"token":proxy.token})).await;
        send(&mut writer, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{"elicitation":{}},"clientInfo":{"name":"fixture","version":"1"}}})).await;
        assert!(receive(&mut reader).await.get("result").is_some());
        send(
            &mut writer,
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        )
        .await;
        send(
            &mut writer,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
        )
        .await;
        let inventory = receive(&mut reader).await;
        assert_eq!(inventory["result"]["tools"].as_array().unwrap().len(), 2);
        assert_eq!(
            proxy.diagnostic(),
            NativeDiagnostic {
                startup: NativeStartup::Ready,
                failed: false
            }
        );
        send(&mut writer, json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"js","arguments":{"code":"not executed"},"_meta":{"x-codex-turn-metadata":{"node_repl_auto_review_required":false}}}})).await;
        let review = receive(&mut reader).await;
        assert_eq!(review["method"], "elicitation/create");
        assert_eq!(proxy.effects(), EffectState::Uncertain);
        send(
            &mut writer,
            json!({"jsonrpc":"2.0","id":review["id"],"result":{"action":"decline"}}),
        )
        .await;
        let result = receive(&mut reader).await;
        assert_eq!(result["result"]["isError"], true);
        assert!(proxy.pending_attention());
        assert_eq!(proxy.effects(), EffectState::Settled);
        let receipt = proxy.shutdown().await;
        assert!(receipt.joined);
        assert!(receipt.pending_attention);
        assert_eq!(receipt.effects, EffectState::Settled);
        assert!(proxy.shutdown().await.joined);
        assert!(!proxy.socket_path().exists());
        assert!(
            store
                .run(&run.id)
                .unwrap()
                .unwrap()
                .capability_processes
                .is_empty()
        );
    }

    #[tokio::test]
    async fn unauthenticated_native_socket_never_starts_connector() {
        let (_directory, server, store, run, workspace) = fixture();
        let mut proxy = NativeMcpProxy::start(server, store.clone(), run.clone(), workspace)
            .await
            .unwrap();
        let stream = UnixStream::connect(proxy.socket_path()).await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        send(&mut writer, json!({"token":"wrong"})).await;
        assert!(
            tokio::time::timeout(
                Duration::from_secs(4),
                read_frame(&mut reader, &mut Vec::new())
            )
            .await
            .unwrap()
            .unwrap()
            .is_none()
        );
        let receipt = proxy.shutdown().await;
        assert!(receipt.joined);
        assert_eq!(receipt.effects, EffectState::None);
        assert_eq!(
            proxy.diagnostic(),
            NativeDiagnostic {
                startup: NativeStartup::WaitingForClient,
                failed: true
            }
        );
        assert!(
            store
                .run(&run.id)
                .unwrap()
                .unwrap()
                .capability_processes
                .is_empty()
        );
    }

    #[test]
    fn strict_review_is_required_without_erasing_context() {
        let mut frame = json!({"params":{"_meta":{"other":"kept","x-codex-turn-metadata":{"threadId":"thread-1","node_repl_auto_review_required":false}}}});
        require_review(&mut frame).unwrap();
        assert_eq!(frame.pointer("/params/_meta/other"), Some(&json!("kept")));
        assert_eq!(
            frame.pointer("/params/_meta/x-codex-turn-metadata/threadId"),
            Some(&json!("thread-1"))
        );
        assert_eq!(
            frame.pointer("/params/_meta/x-codex-turn-metadata/node_repl_auto_review_required"),
            Some(&json!(true))
        );
        assert!(require_review(&mut json!({"params":{"_meta":false}})).is_err());
        assert!(
            require_review(&mut json!({"params":{"_meta":{"x-codex-turn-metadata":"wrong"}}}))
                .is_err()
        );
    }

    #[test]
    fn ids_preserve_type_and_refuse_unbounded_or_structured_values() {
        assert_ne!(rpc_id(&json!(1)).unwrap(), rpc_id(&json!("1")).unwrap());
        for invalid in [
            Value::Null,
            json!({}),
            json!(1.5),
            json!(""),
            json!("a".repeat(161)),
        ] {
            assert!(rpc_id(&invalid).is_err());
        }
    }
}
