use super::{
    bridge::{DevinBridge, Request},
    config::NATIVE_TOOLS,
};
use crate::{
    Error, Result, broker, now_ms,
    process::StreamProcess,
    protocol::{Batch, Event, Prompt, Protocol},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};
use tokio::sync::oneshot;
use xcb_core::{
    Id, MAX_JSON_BYTES, MAX_TEXT_BYTES, Provider,
    models::{Mode, ModelChoice},
    policy::Terminal,
    usage::{COUNTER_LIMIT, Counters},
};

const MAX_CALLS: usize = 128;
const MAX_FRAMES: usize = 16384;
// Match the native catalog/store bound; the observed live catalog has 385 choices.
const MAX_MODEL_CHOICES: usize = 4096;
// ACP can carry the same 10 MiB image accepted by the attachment importer.
// MCP messages retain the smaller broker JSON bound.
const MAX_WIRE_BYTES: usize = 16 * 1024 * 1024;
const MAX_IMAGE_BASE64: usize = (10 * 1024 * 1024_usize).div_ceil(3) * 4;
const MAX_PROMPT_BYTES: usize = 1024 * 1024;

pub(crate) struct DevinOptions {
    /// Disposable empty process directory, never the consumer workspace.
    pub cwd: PathBuf,
    pub model: ModelChoice,
    pub tools: bool,
    pub metadata_only: bool,
}

struct Call {
    name: String,
    arguments: Value,
    approved: bool,
    bridged: bool,
    replied: bool,
    finished: bool,
}
struct Pending {
    rpc_id: Value,
    reply: oneshot::Sender<Option<Value>>,
}

pub(crate) struct DevinProtocol {
    options: DevinOptions,
    bridge: Option<DevinBridge>,
    session: Option<String>,
    next_id: u64,
    frames: usize,
    prompt_id: Option<u64>,
    instructions: String,
    ready: bool,
    announced: bool,
    completed: bool,
    text: String,
    calls: BTreeMap<String, Call>,
    pending: BTreeMap<String, Pending>,
    callback_ids: BTreeSet<String>,
    mcp_ids: BTreeSet<String>,
    broker_names: BTreeSet<String>,
    mcp_initialized: bool,
    #[cfg(test)]
    mcp_proposed_version: Option<String>,
    #[cfg(test)]
    mcp_metadata_seen: bool,
    listed: bool,
    output_tokens: u64,
}

fn require(ok: bool, reason: &'static str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(Error::Protocol(reason))
    }
}
// Provider error messages/data may contain credentials or private paths. Only
// the host-selected operation, numeric code, and fixed category can leave here.
fn initialization_response(value: &Value, id: u64, method: &'static str) -> Result<Value> {
    require(
        value["id"].as_u64() == Some(id),
        "Devin initialization response identity",
    )?;
    if let Some(error) = value.get("error") {
        let message = error["message"].as_str().unwrap_or("");
        let message = message
            .chars()
            .take(8192)
            .collect::<String>()
            .to_ascii_lowercase();
        let contains = |words: &[&str]| words.iter().any(|word| message.contains(word));
        // This exact code/kind is defined by the admitted Devin runtime. Do
        // not infer a balance, reset time, or account/model scope from it.
        let resource_limit = error["code"].as_i64() == Some(-32011)
            || error["data"]["cognition.ai/errorKind"].as_str() == Some("resource_exhausted");
        let category = if resource_limit {
            "provider quota or resource limit reached"
        } else if contains(&["certificate", "tls", "ssl"]) {
            "TLS certificate or transport failure"
        } else if contains(&[
            "unauthorized",
            "unauthenticated",
            "authentication",
            "401",
            "expired token",
            "invalid token",
        ]) || error["code"].as_i64() == Some(-32000) && message.contains("auth required")
        {
            "authentication rejected; reconnect this account"
        } else if contains(&["permission denied", "operation not permitted"]) {
            "local provider access denied"
        } else if contains(&[
            "connect",
            "network",
            "dns",
            "request",
            "timed out",
            "timeout",
        ]) {
            "provider request or network failure"
        } else {
            "provider rejected the operation"
        };
        return Err(Error::DevinRpc {
            method,
            code: error["code"].as_i64().unwrap_or(-32603),
            category,
        });
    }
    Ok(value["result"].clone())
}

fn text(value: &Value, max: usize) -> Result<&str> {
    value
        .as_str()
        .filter(|s| s.len() <= max)
        .ok_or(Error::Protocol("Devin text bound"))
}
fn identity(value: &Value) -> Result<String> {
    let s = text(value, 160)?;
    require(
        !s.is_empty() && !s.chars().any(char::is_control),
        "Devin identity",
    )?;
    Ok(s.to_owned())
}
fn rpc_key(id: &Value) -> Result<String> {
    if id.is_string() {
        identity(id)?;
    } else {
        require(id.as_i64().is_some(), "Devin RPC identity")?;
    }
    Ok(serde_json::to_string(id)?)
}
fn closed(value: &Value, keys: &[&str]) -> Result<()> {
    let o = value.as_object().ok_or(Error::Protocol("Devin object"))?;
    require(
        o.len() <= 256 && o.keys().all(|k| keys.contains(&k.as_str())),
        "Devin unknown field",
    )
}
fn counter(value: &Value) -> Result<u64> {
    value
        .as_u64()
        .filter(|n| *n <= COUNTER_LIMIT)
        .ok_or(Error::Protocol("Devin token counter"))
}

pub fn parse_models(result: &Value, observed_at_ms: u64) -> Result<Vec<ModelChoice>> {
    let options = result["configOptions"]
        .as_array()
        .filter(|v| v.len() <= 32)
        .ok_or(Error::Protocol("Devin config options"))?;
    let models: Vec<_> = options.iter().filter(|v| v["id"] == "model").collect();
    require(
        models.len() == 1 && models[0]["type"] == "select",
        "Devin model option",
    )?;
    let choices = models[0].get("options");
    let rows = choices
        .and_then(Value::as_array)
        .filter(|v| v.len() <= MAX_MODEL_CHOICES)
        .ok_or_else(|| Error::DevinModelChoices {
            shape: match choices {
                None => "missing",
                Some(Value::Null) => "null",
                Some(Value::Bool(_)) => "boolean",
                Some(Value::Number(_)) => "number",
                Some(Value::String(_)) => "string",
                Some(Value::Array(_)) => "array",
                Some(Value::Object(_)) => "object",
            },
            count: choices.and_then(Value::as_array).map(Vec::len),
        })?;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for row in rows {
        let id = identity(&row["value"])?;
        require(seen.insert(id.clone()), "Devin duplicate model")?;
        let model = ModelChoice {
            provider: Provider::Devin,
            id: Id::new(id)?,
            label: text(&row["name"], 256)?.to_owned(),
            mode: Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms,
        };
        model.validate()?;
        out.push(model);
    }
    Ok(out)
}

impl DevinProtocol {
    #[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
    pub(crate) fn new(options: DevinOptions, bridge: Option<DevinBridge>) -> Result<Self> {
        options.model.validate()?;
        require(
            options.model.provider == Provider::Devin
                && options.model.mode == Mode::Fixed
                && options.model.effort.is_none(),
            "Devin model mode",
        )?;
        require(
            options.cwd.is_absolute()
                && options
                    .cwd
                    .to_str()
                    .is_some_and(|s| s.len() <= 4096 && !s.chars().any(char::is_control)),
            "Devin disposable cwd",
        )?;
        require(
            options.tools == bridge.is_some(),
            "Devin broker configuration",
        )?;
        let broker_names = broker::descriptors()
            .iter()
            .map(|v| identity(&v["name"]))
            .collect::<Result<_>>()?;
        Ok(Self {
            options,
            bridge,
            session: None,
            next_id: 0,
            frames: 0,
            prompt_id: None,
            instructions: String::new(),
            ready: false,
            announced: false,
            completed: false,
            text: String::new(),
            calls: BTreeMap::new(),
            pending: BTreeMap::new(),
            callback_ids: BTreeSet::new(),
            mcp_ids: BTreeSet::new(),
            broker_names,
            mcp_initialized: false,
            #[cfg(test)]
            mcp_proposed_version: None,
            #[cfg(test)]
            mcp_metadata_seen: false,
            listed: false,
            output_tokens: 0,
        })
    }
    fn prompt_wire(&self, prompt: Prompt, id: u64) -> Result<Value> {
        if prompt.text.len() > MAX_PROMPT_BYTES {
            return Err(Error::Unavailable(
                "Devin conversation is too large; compact retained context or start a new session",
            ));
        }
        let text = format!(
            "{}\n\nUse only the xcb MCP server for workspace access. Native tools have no workspace authority. List the xcb tools before calling them.\n\n{}",
            self.instructions, prompt.text
        );
        let mut content = vec![json!({"type":"text","text":text})];
        for image in prompt.images {
            if !matches!(
                image.media_type.as_str(),
                "image/png" | "image/jpeg" | "image/webp"
            ) || image.base64.len() > MAX_IMAGE_BASE64
            {
                return Err(Error::Unavailable(
                    "Devin images must be PNG, JPEG, or WebP and at most 10 MiB each",
                ));
            }
            content.push(json!({"type":"image","mimeType":image.media_type,"data":image.base64}));
        }
        let wire = json!({"jsonrpc":"2.0","id":id,"method":"session/prompt","params":{"sessionId":self.session,"prompt":content}});
        if serde_json::to_vec(&wire)?.len() > MAX_WIRE_BYTES {
            return Err(Error::Unavailable(
                "Devin request exceeds 16 MiB; remove an image or reduce its size",
            ));
        }
        Ok(wire)
    }
    fn session_scope(&self, p: &Value) -> Result<()> {
        require(
            self.session
                .as_deref()
                .is_some_and(|s| p["sessionId"].as_str() == Some(s)),
            "Devin session scope",
        )
    }
    fn envelope(&mut self, bytes: &[u8]) -> Result<Value> {
        self.frames += 1;
        require(
            self.frames <= MAX_FRAMES && bytes.len() <= MAX_WIRE_BYTES,
            "Devin frame bound",
        )?;
        let value: Value = serde_json::from_slice(bytes)?;
        closed(
            &value,
            &["jsonrpc", "id", "method", "params", "result", "error"],
        )?;
        require(value["jsonrpc"] == "2.0", "Devin protocol version")?;
        if value.get("method").is_some() {
            identity(&value["method"])?;
            require(
                value.get("result").is_none() && value.get("error").is_none(),
                "Devin mixed request response",
            )?;
        } else {
            require(
                value.get("id").is_some()
                    && (value.get("result").is_some() != value.get("error").is_some()),
                "Devin malformed response",
            )?;
        }
        if let Some(id) = value.get("id") {
            rpc_key(id)?;
        }
        Ok(value)
    }
    async fn request(
        &mut self,
        process: &mut StreamProcess,
        method: &'static str,
        params: Value,
    ) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        process
            .send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        for _ in 0..1024 {
            let packet = self.packet(process).await?;
            match packet {
                Packet::Mcp(request) => {
                    let events = self.mcp(request)?;
                    require(events.is_empty(), "Devin tool during initialization")?;
                }
                Packet::Acp(bytes) => {
                    let v = self.envelope(&bytes)?;
                    if v.get("method").is_none() {
                        return initialization_response(&v, id, method);
                    }
                    let (events, replies) = self.accept(v)?;
                    require(events.is_empty(), "Devin early event")?;
                    for reply in replies {
                        process.send(&reply).await?;
                    }
                }
            }
        }
        Err(Error::Protocol("Devin initialization frame bound"))
    }
    async fn packet(&mut self, process: &mut StreamProcess) -> Result<Packet> {
        if let Some(bridge) = &mut self.bridge {
            tokio::select! {
                bytes=process.frame_bounded(MAX_WIRE_BYTES)=>Ok(Packet::Acp(bytes?.ok_or(Error::Protocol("Devin ended before result"))?)),
                request=bridge.receiver.recv()=>Ok(Packet::Mcp(request.ok_or(Error::Protocol("Devin bridge ended"))?)),
            }
        } else {
            Ok(Packet::Acp(
                process
                    .frame_bounded(MAX_WIRE_BYTES)
                    .await?
                    .ok_or(Error::Protocol("Devin ended before result"))?,
            ))
        }
    }
    fn validate_options(&self, result: &Value) -> Result<()> {
        let options = result["configOptions"]
            .as_array()
            .filter(|v| v.len() <= 32)
            .ok_or(Error::Protocol("Devin options absent"))?;
        let mut seen = BTreeSet::new();
        let mut model = false;
        let mut mode = false;
        for option in options {
            let id = identity(&option["id"])?;
            require(seen.insert(id.clone()), "Devin duplicate config option")?;
            match id.as_str() {
                "model" => {
                    require(
                        option["currentValue"] == self.options.model.id.as_str(),
                        "Devin model changed",
                    )?;
                    model = true;
                }
                "mode" => {
                    require(
                        option["currentValue"] == "accept-edits",
                        "Devin mode changed",
                    )?;
                    mode = true;
                }
                _ => (),
            }
        }
        require(model && mode, "Devin model/mode metadata missing")
    }
    fn permission(&mut self, p: &Value) -> Result<bool> {
        self.session_scope(p)?;
        require(
            self.ready && !self.completed,
            "Devin permission outside turn",
        )?;
        let id = identity(&p["toolCall"]["toolCallId"])?;
        let options = p["options"]
            .as_array()
            .filter(|v| v.len() <= 32)
            .ok_or(Error::Protocol("Devin permission options"))?;
        let mut seen = BTreeSet::new();
        for option in options {
            require(
                seen.insert(identity(&option["optionId"])?),
                "Devin duplicate permission choice",
            )?;
        }
        let allow = options
            .iter()
            .any(|o| o["optionId"] == "allow_once" && o["kind"] == "allow_once");
        let Some(call) = self.calls.get_mut(&id) else {
            return Ok(false);
        };
        if self.options.tools
            && self.broker_names.contains(&call.name)
            && !call.approved
            && !call.finished
            && allow
        {
            call.approved = true;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    /// Pure ACP transition. A model can name a native tool, but this creates
    /// no host capability. Any completed native effect is a protocol failure.
    fn accept(&mut self, v: Value) -> Result<(Vec<Event>, Vec<Value>)> {
        let mut events = Vec::new();
        let mut outgoing = Vec::new();
        if v.get("method").is_none() {
            require(
                self.ready && !self.completed && v["id"].as_u64() == self.prompt_id,
                "Devin prompt response identity",
            )?;
            let r = initialization_response(
                &v,
                self.prompt_id
                    .ok_or(Error::Protocol("Devin prompt response identity"))?,
                "session/prompt",
            )?;
            let terminal = match r["stopReason"].as_str() {
                Some("end_turn") => Terminal::Completed,
                Some("max_tokens") => Terminal::TokenLimit,
                Some("max_turn_requests") => Terminal::TurnLimit,
                Some("cancelled") => Terminal::Cancelled,
                Some("refusal") => Terminal::Failed,
                _ => return Err(Error::Protocol("Devin stop reason")),
            };
            require(
                self.pending.is_empty()
                    && self
                        .calls
                        .values()
                        .all(|c| !c.approved || (c.bridged && c.replied && c.finished)),
                "Devin result before broker settlement",
            )?;
            let mut models = Vec::new();
            if let Some(u) = r.get("usage").filter(|v| !v.is_null()) {
                let input = counter(&u["inputTokens"])?;
                let output = counter(&u["outputTokens"])?;
                let total = counter(&u["totalTokens"])?;
                require(
                    input.checked_add(output) == Some(total) && output >= self.output_tokens,
                    "Devin usage mismatch",
                )?;
                self.output_tokens = output;
                events.push(Event::OutputTokens(output));
                models.push((
                    self.options.model.id.to_string(),
                    Counters {
                        input,
                        output,
                        ..Counters::default()
                    },
                ));
            }
            self.completed = true;
            events.push(Event::Result {
                terminal,
                text: self.text.clone(),
                models,
            });
            return Ok((events, outgoing));
        }
        let method = identity(&v["method"])?;
        let p = &v["params"];
        if let Some(id) = v.get("id") {
            require(
                self.callback_ids.len() < 1024 && self.callback_ids.insert(rpc_key(id)?),
                "Devin duplicate callback",
            )?;
            if method == "session/request_permission" {
                let allow = self.permission(p)?;
                outgoing.push(json!({"jsonrpc":"2.0","id":id,"result":{"outcome":if allow {json!({"outcome":"selected","optionId":"allow_once"})}else{json!({"outcome":"cancelled"})}}}));
                if !allow {
                    events.push(Event::Attention);
                }
            } else {
                outgoing.push(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"host capability unavailable"}}));
                events.push(Event::Attention);
            }
            return Ok((events, outgoing));
        }
        if method == "session/update" {
            if self.session.is_none() {
                return Ok((events, outgoing));
            }
            self.session_scope(p)?;
            let u = &p["update"];
            match u["sessionUpdate"].as_str() {
                Some("user_message_chunk") => {
                    require(self.ready && !self.completed, "Devin echo outside turn")?;
                    require(
                        matches!(u["content"]["type"].as_str(), Some("text" | "image")),
                        "Devin unsupported input echo",
                    )?;
                }
                Some("agent_message_chunk" | "agent_thought_chunk") => {
                    require(self.ready && !self.completed, "Devin text outside turn")?;
                    require(
                        u["content"]["type"] == "text",
                        "Devin unsupported output content",
                    )?;
                    let delta = text(&u["content"]["text"], MAX_TEXT_BYTES)?.to_owned();
                    let thinking = u["sessionUpdate"] == "agent_thought_chunk";
                    if !thinking {
                        require(
                            self.text.len() + delta.len() <= MAX_TEXT_BYTES,
                            "Devin text total bound",
                        )?;
                        self.text.push_str(&delta);
                    }
                    events.push(Event::Delta {
                        thinking,
                        text: delta,
                    });
                }
                Some("tool_call") => {
                    require(
                        self.ready && !self.completed && self.calls.len() < MAX_CALLS,
                        "Devin tool outside turn",
                    )?;
                    let id = identity(&u["toolCallId"])?;
                    require(
                        !self.calls.contains_key(&id),
                        "Devin duplicate tool declaration",
                    )?;
                    let m = &u["_meta"];
                    let inference = identity(&m["cognition.ai/inferenceToolName"])?;
                    let name = if let Some(name) = inference.strip_prefix("mcp__xcb__") {
                        require(
                            self.options.tools
                                && self.broker_names.contains(name)
                                && m["cognition.ai/toolName"] == inference
                                && m["cognition.ai/eventType"] == "mcp_tool_call",
                            "Devin forged broker declaration",
                        )?;
                        name.to_owned()
                    } else {
                        require(
                            NATIVE_TOOLS.contains(&inference.as_str())
                                || inference.starts_with("mcp__"),
                            "Devin unqualified native inventory",
                        )?;
                        inference
                    };
                    let arguments = u.get("rawInput").cloned().unwrap_or_else(|| json!({}));
                    require(arguments.is_object(), "Devin tool argument object")?;
                    self.calls.insert(
                        id,
                        Call {
                            name,
                            arguments,
                            approved: false,
                            bridged: false,
                            replied: false,
                            finished: false,
                        },
                    );
                }
                Some("tool_call_update") => {
                    let id = identity(&u["toolCallId"])?;
                    let call = self
                        .calls
                        .get_mut(&id)
                        .ok_or(Error::Protocol("Devin orphan tool update"))?;
                    require(!call.finished, "Devin update after tool completion")?;
                    if let Some(input) = u.get("rawInput") {
                        require(input == &call.arguments, "Devin tool arguments changed")?;
                    }
                    if let Some(name) = u
                        .pointer("/_meta/cognition.ai~1inferenceToolName")
                        .and_then(Value::as_str)
                    {
                        require(
                            name == call.name || name == format!("mcp__xcb__{}", call.name),
                            "Devin tool identity changed",
                        )?;
                    }
                    match u["status"].as_str() {
                        Some("completed") => {
                            require(
                                (call.approved && call.bridged && call.replied)
                                    || ["mcp_list_tools", "mcp_list_servers"]
                                        .contains(&call.name.as_str()),
                                "Devin native tool executed",
                            )?;
                            call.finished = true;
                        }
                        Some("failed") => {
                            require(
                                !call.approved || call.replied,
                                "Devin broker failed before reply",
                            )?;
                            call.finished = true;
                        }
                        Some("pending" | "in_progress") | None => (),
                        _ => return Err(Error::Protocol("Devin tool status")),
                    }
                }
                Some("config_option_update") if self.ready => self.validate_options(u)?,
                Some("config_option_update") => (),
                Some("current_mode_update") => {
                    require(u["currentModeId"] == "accept-edits", "Devin mode drift")?
                }
                Some("usage_update") => {
                    if let Some(output) = u.get("outputTokens").filter(|v| !v.is_null()) {
                        let count = counter(output)?;
                        require(count >= self.output_tokens, "Devin usage regressed")?;
                        self.output_tokens = count;
                        events.push(Event::OutputTokens(count));
                    }
                }
                Some("available_commands_update" | "session_info_update" | "plan") => (),
                _ => return Err(Error::Protocol("Devin unknown session update")),
            }
        } else if !matches!(
            method.as_str(),
            "_cognition.ai/mcp/serversChanged"
                | "_cognition.ai/output"
                | "_cognition.ai/turn_stats"
                | "_cognition.ai/agent_stopped"
                | "_cognition.ai/sessionChanged"
                | "_cognition.ai/cwdChanged"
        ) {
            return Err(Error::Protocol("Devin unsupported notification"));
        }
        Ok((events, outgoing))
    }
    fn mcp(&mut self, request: Request) -> Result<Vec<Event>> {
        let v = &request.value;
        closed(v, &["jsonrpc", "id", "method", "params"])?;
        require(
            v["jsonrpc"] == "2.0" && self.options.tools,
            "Devin MCP protocol",
        )?;
        let method = identity(&v["method"])?;
        if method == "notifications/initialized" {
            require(
                self.mcp_initialized && v.get("id").is_none(),
                "Devin MCP notification",
            )?;
            request
                .reply
                .send(None)
                .map_err(|_| Error::Protocol("Devin MCP reply channel"))?;
            return Ok(vec![]);
        }
        let id = v
            .get("id")
            .ok_or(Error::Protocol("Devin MCP request identity"))?
            .clone();
        require(
            self.mcp_ids.len() < 4096 && self.mcp_ids.insert(rpc_key(&id)?),
            "Devin MCP duplicate request",
        )?;
        let result = match method.as_str() {
            "initialize" => {
                require(!self.mcp_initialized, "Devin MCP reinitialization")?;
                let proposed = text(&v["params"]["protocolVersion"], 32)?;
                require(
                    proposed.len() == 10
                        && proposed.bytes().enumerate().all(|(i, b)| {
                            if i == 4 || i == 7 {
                                b == b'-'
                            } else {
                                b.is_ascii_digit()
                            }
                        }),
                    "Devin MCP version",
                )?;
                // MCP lifecycle negotiation requires a server-supported
                // version response when the client's proposal differs. This
                // selects only our known protocol, never new capabilities.
                let version = match proposed {
                    "2024-11-05" | "2025-03-26" | "2025-06-18" => proposed,
                    _ => "2025-06-18",
                };
                #[cfg(test)]
                {
                    self.mcp_proposed_version = Some(proposed.to_owned());
                }
                self.mcp_initialized = true;
                json!({"protocolVersion":version,"capabilities":{"tools":{}},"serverInfo":{"name":"xcb","version":env!("CARGO_PKG_VERSION")}})
            }
            "tools/list" => {
                require(self.mcp_initialized, "Devin MCP uninitialized")?;
                self.listed = true;
                json!({"tools":broker::descriptors()})
            }
            "ping" => json!({}),
            "tools/call" => {
                require(
                    self.ready && !self.completed && self.mcp_initialized && self.listed,
                    "Devin MCP call before admission",
                )?;
                closed(&v["params"], &["name", "arguments", "_meta"])?;
                if let Some(meta) = v["params"].get("_meta") {
                    // Standard request progress metadata is transport-only.
                    // It never becomes a tool argument or an approval grant.
                    closed(meta, &["progressToken"])?;
                    if let Some(token) = meta.get("progressToken") {
                        rpc_key(token)?;
                    }
                    #[cfg(test)]
                    {
                        self.mcp_metadata_seen = true;
                    }
                }
                let name = identity(&v["params"]["name"])?;
                let arguments = v["params"]["arguments"].clone();
                require(
                    self.broker_names.contains(&name) && arguments.is_object(),
                    "Devin MCP tool authority",
                )?;
                let matches: Vec<_> = self
                    .calls
                    .iter()
                    .filter(|(_, c)| {
                        c.approved
                            && !c.bridged
                            && !c.finished
                            && c.name == name
                            && c.arguments == arguments
                    })
                    .map(|(id, _)| id.clone())
                    .collect();
                require(matches.len() == 1, "Devin MCP approval correlation")?;
                let call_id = matches[0].clone();
                self.calls.get_mut(&call_id).expect("matched call").bridged = true;
                self.pending.insert(
                    call_id.clone(),
                    Pending {
                        rpc_id: id,
                        reply: request.reply,
                    },
                );
                return Ok(vec![Event::Tool {
                    id: call_id,
                    name,
                    arguments,
                }]);
            }
            _ => return Err(Error::Protocol("Devin unsupported MCP method")),
        };
        request
            .reply
            .send(Some(json!({"jsonrpc":"2.0","id":id,"result":result})))
            .map_err(|_| Error::Protocol("Devin MCP reply channel"))?;
        Ok(vec![])
    }
}

enum Packet {
    Acp(Vec<u8>),
    Mcp(Request),
}

impl Protocol for DevinProtocol {
    async fn initialize(
        &mut self,
        process: &mut StreamProcess,
        instructions: &str,
    ) -> Result<Vec<ModelChoice>> {
        require(
            instructions.len() <= MAX_TEXT_BYTES,
            "Devin instructions bound",
        )?;
        self.instructions = instructions.to_owned();
        let init=self.request(process,"initialize",json!({"protocolVersion":1,"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false},"clientInfo":{"name":"xcb","version":env!("CARGO_PKG_VERSION")}})).await?;
        require(
            init["protocolVersion"] == 1 && init["agentInfo"]["name"] == "affogato",
            "Devin ACP implementation",
        )?;
        let result = self
            .request(
                process,
                "session/new",
                json!({"cwd":self.options.cwd,"mcpServers":[]}),
            )
            .await?;
        self.session = Some(identity(&result["sessionId"])?);
        let models = parse_models(&result, now_ms())?;
        if self.options.metadata_only {
            return Ok(models);
        }
        let selected = if result["configOptions"].as_array().is_some_and(|options| {
            options
                .iter()
                .any(|o| o["id"] == "model" && o["currentValue"] == self.options.model.id.as_str())
        }) {
            result
        } else {
            self.request(process,"session/set_config_option",json!({"sessionId":self.session,"configId":"model","value":self.options.model.id.as_str()})).await?
        };
        // Setting the model must echo the complete current mode/model metadata.
        self.validate_options(&selected)?;
        let mode = self
            .request(
                process,
                "session/set_mode",
                json!({"sessionId":self.session,"modeId":"accept-edits"}),
            )
            .await?;
        require(mode.is_object(), "Devin mode acknowledgment")?;
        Ok(models)
    }
    async fn start(&mut self, process: &mut StreamProcess, prompt: Prompt) -> Result<()> {
        require(
            !self.ready && !self.completed && self.session.is_some() && !self.options.metadata_only,
            "Devin turn start",
        )?;
        let wire = self.prompt_wire(prompt, self.next_id + 1)?;
        self.next_id += 1;
        self.prompt_id = Some(self.next_id);
        self.ready = true;
        process.send(&wire).await
    }
    async fn next(&mut self, process: &mut StreamProcess) -> Result<Batch> {
        // The host needs its admission event before it can accept tool effects.
        let packet = self.packet(process).await?;
        let mut batch = match packet {
            Packet::Acp(bytes) => Batch {
                bytes: bytes.len(),
                events: self.receive(process, &bytes).await?,
            },
            Packet::Mcp(request) => {
                let bytes = request.bytes;
                Batch {
                    bytes,
                    events: self.mcp(request)?,
                }
            }
        };
        if self.ready && !self.announced {
            self.announced = true;
            batch.events.insert(
                0,
                Event::Ready {
                    resolved_model: Some(self.options.model.id.to_string()),
                },
            );
        }
        Ok(batch)
    }
    async fn receive(&mut self, process: &mut StreamProcess, frame: &[u8]) -> Result<Vec<Event>> {
        let v = self.envelope(frame)?;
        let (events, replies) = self.accept(v)?;
        for reply in replies {
            process.send(&reply).await?;
        }
        Ok(events)
    }
    async fn reply(&mut self, _: &mut StreamProcess, id: &str, result: Value) -> Result<()> {
        require(
            serde_json::to_vec(&result)?.len() <= MAX_JSON_BYTES,
            "Devin broker result bound",
        )?;
        let pending = self
            .pending
            .remove(id)
            .ok_or(Error::Protocol("Devin unsolicited tool reply"))?;
        let call = self
            .calls
            .get_mut(id)
            .ok_or(Error::Protocol("Devin tool reply identity"))?;
        require(
            call.approved && call.bridged && !call.replied && !call.finished,
            "Devin tool reply state",
        )?;
        pending
            .reply
            .send(Some(
                json!({"jsonrpc":"2.0","id":pending.rpc_id,"result":result}),
            ))
            .map_err(|_| Error::Protocol("Devin tool reply channel closed"))?;
        call.replied = true;
        Ok(())
    }
    async fn shutdown(&mut self) -> bool {
        self.pending.clear();
        match &mut self.bridge {
            Some(bridge) => bridge.shutdown().await,
            None => true,
        }
    }
}

#[cfg(test)]
mod tests;
