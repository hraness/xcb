//! A small async client over the frozen `xcb.protocol.v1` frame contract.
//!
//! The client has no socket opinion: a host supplies [`ProtocolTransport`].
//! The reference factory below is an in-memory, deterministic transport used
//! by examples, tests, and projection consumers that need no terminal UI.

use crate::{Error, Result, projections, protocol};
use projections::{
    Capability, CapabilitySet, EVENT_PROJECTION_SCHEMA, EventPage, EventProjection, EventQuery,
    FactoryProjection, MAX_EVENT_PAGE_SIZE, STATUS_PROJECTION_SCHEMA, StatusProjection,
    TaskProjection, redact_event, redact_status, schema_digest, validate_event_page,
    validate_status,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// The transport boundary is deliberately narrower than a socket.  A native
/// stdio, Unix socket, Valhalla lane, or test double can all implement it
/// without changing command validation or projection redaction.
pub trait ProtocolTransport: Send + Sync {
    fn round_trip<'a>(&'a self, frame: Vec<u8>) -> BoxFuture<'a, Vec<u8>>;
    fn status<'a>(&'a self, capabilities: CapabilitySet) -> BoxFuture<'a, StatusProjection>;
    fn events<'a>(&'a self, query: EventQuery) -> BoxFuture<'a, EventPage>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientOptions {
    pub client_name: String,
    pub client_version: String,
    pub capabilities: CapabilitySet,
    pub expected_schema_digest: String,
}
impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            client_name: "xcb-rust-client".into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
            capabilities: CapabilitySet::public(),
            expected_schema_digest: schema_digest(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandReceipt {
    pub status: String,
    pub receipt_id: String,
    pub revision: Option<String>,
}

/// A typed command before request IDs are assigned.  Builders validate the
/// command name and JSON argument bounds again when the wire frame is built.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TypedCommand {
    pub command: String,
    pub arguments: Value,
    pub expected_revision: Option<String>,
    pub idempotency_key: Option<String>,
}
impl TypedCommand {
    pub fn builder(command: impl Into<String>) -> Result<CommandBuilder> {
        CommandBuilder::new(command)
    }
    pub fn frame(
        &self,
        request_id: impl Into<String>,
        idempotency_key: impl Into<String>,
    ) -> Result<protocol::Frame> {
        protocol::command_submit_request(
            request_id,
            self.command.clone(),
            self.arguments.clone(),
            self.expected_revision.clone(),
            self.idempotency_key
                .clone()
                .unwrap_or_else(|| idempotency_key.into()),
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandBuilder {
    command: String,
    arguments: Map<String, Value>,
    expected_revision: Option<String>,
    idempotency_key: Option<String>,
}
impl CommandBuilder {
    pub fn new(command: impl Into<String>) -> Result<Self> {
        let command = command.into();
        if command.is_empty()
            || command.len() > 96
            || !command.as_bytes()[0].is_ascii_alphabetic()
            || !command
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._/-".contains(&byte))
        {
            return Err(Error::Invalid("command"));
        }
        Ok(Self {
            command,
            arguments: Map::new(),
            expected_revision: None,
            idempotency_key: None,
        })
    }
    pub fn argument(mut self, name: impl Into<String>, value: impl Serialize) -> Result<Self> {
        let name = name.into();
        if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
            return Err(Error::Invalid("command argument name"));
        }
        let value = serde_json::to_value(value).map_err(|_| Error::Invalid("command argument"))?;
        self.arguments.insert(name, value);
        Ok(self)
    }
    pub fn arguments(mut self, value: Value) -> Result<Self> {
        self.arguments = value
            .as_object()
            .cloned()
            .ok_or(Error::Invalid("command arguments"))?;
        Ok(self)
    }
    pub fn expected_revision(mut self, value: Option<impl Into<String>>) -> Self {
        self.expected_revision = value.map(Into::into);
        self
    }
    pub fn idempotency_key(mut self, value: impl Into<String>) -> Self {
        self.idempotency_key = Some(value.into());
        self
    }
    pub fn build(self) -> Result<TypedCommand> {
        let command = TypedCommand {
            command: self.command,
            arguments: Value::Object(self.arguments),
            expected_revision: self.expected_revision,
            idempotency_key: self.idempotency_key,
        };
        // A deterministic placeholder is accepted only as a shape check; the
        // client replaces it with a unique key when none was provided.
        command
            .frame("req_builder", "idem_builder")
            .map(|_| command)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskRunInput {
    pub task_id: String,
    pub prompt: String,
    pub workspace: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
}

pub fn task_run(input: TaskRunInput) -> Result<TypedCommand> {
    let mut builder = CommandBuilder::new("task/run")?
        .argument("taskId", input.task_id)?
        .argument("prompt", input.prompt)?;
    if let Some(workspace) = input.workspace {
        builder = builder.argument("workspace", workspace)?;
    }
    if let Some(provider) = input.provider {
        builder = builder.argument("provider", provider)?;
    }
    if let Some(model) = input.model {
        builder = builder.argument("model", model)?;
    }
    builder.build()
}

pub fn task_cancel(
    task_id: impl Into<String>,
    expected_revision: impl Into<String>,
) -> Result<TypedCommand> {
    CommandBuilder::new("task/cancel")?
        .argument("taskId", task_id.into())?
        .expected_revision(Some(expected_revision.into()))
        .build()
}

pub fn status_read() -> Result<TypedCommand> {
    CommandBuilder::new("status/read")?.build()
}

pub fn events_read(query: &EventQuery) -> Result<TypedCommand> {
    query.validate()?;
    CommandBuilder::new("events/read")?
        .argument("limit", query.limit as u64)?
        .arguments(query_to_value(query)?)?
        .build()
}
fn query_to_value(query: &EventQuery) -> Result<Value> {
    let mut value = Map::new();
    value.insert("limit".into(), json!(query.limit));
    if let Some(cursor) = &query.cursor {
        value.insert("cursor".into(), json!(cursor));
    }
    if let Some(entity_id) = &query.entity_id {
        value.insert("entityId".into(), json!(entity_id));
    }
    Ok(Value::Object(value))
}

pub struct XcbClient<T> {
    transport: T,
    options: ClientOptions,
    request_counter: AtomicU64,
    initialized: std::sync::atomic::AtomicBool,
}
impl<T: ProtocolTransport> XcbClient<T> {
    pub fn new(transport: T, options: ClientOptions) -> Result<Self> {
        projections::check_schema_digest(&options.expected_schema_digest)?;
        options.capabilities.validate()?;
        Ok(Self {
            transport,
            options,
            request_counter: AtomicU64::new(1),
            initialized: std::sync::atomic::AtomicBool::new(false),
        })
    }
    pub fn transport(&self) -> &T {
        &self.transport
    }
    pub fn options(&self) -> &ClientOptions {
        &self.options
    }
    fn request_id(&self) -> String {
        format!(
            "req_sdk_{}",
            self.request_counter.fetch_add(1, Ordering::Relaxed)
        )
    }
    pub async fn initialize(&self) -> Result<protocol::InitializeResult> {
        let request = protocol::initialize_request(
            self.request_id(),
            self.options.client_name.clone(),
            self.options.client_version.clone(),
            protocol::Capabilities {
                versions: vec![protocol::SCHEMA.into()],
                features: protocol::FEATURES
                    .iter()
                    .map(|value| (*value).into())
                    .collect(),
            },
        )?;
        let request_id = match &request {
            protocol::Frame::Request(value) => value.request_id.clone(),
            protocol::Frame::Response(_) => return Err(Error::Invalid("initialize request")),
        };
        let bytes = self
            .transport
            .round_trip(protocol::encode_frame(&request)?)
            .await?;
        let response = protocol::decode_frame(&bytes)?;
        let protocol::Frame::Response(response) = response else {
            return Err(Error::Invalid("initialize response"));
        };
        if response.request_id != request_id || !response.ok {
            return Err(Error::Invalid("initialize response"));
        }
        let value = response.result.ok_or(Error::Invalid("initialize result"))?;
        let result: protocol::InitializeResult =
            serde_json::from_value(value).map_err(|_| Error::Invalid("initialize result"))?;
        if result.protocol_version != protocol::SCHEMA {
            return Err(Error::Invalid("protocol version"));
        }
        self.initialized.store(true, Ordering::Release);
        Ok(result)
    }
    pub async fn submit(&self, command: TypedCommand) -> Result<CommandReceipt> {
        let request_id = self.request_id();
        let idempotency_key = command.idempotency_key.clone().unwrap_or_else(|| {
            format!(
                "idem_sdk_{}",
                self.request_counter.fetch_add(1, Ordering::Relaxed)
            )
        });
        let request = command.frame(request_id.clone(), idempotency_key)?;
        let bytes = self
            .transport
            .round_trip(protocol::encode_frame(&request)?)
            .await?;
        let response = protocol::decode_frame(&bytes)?;
        let protocol::Frame::Response(response) = response else {
            return Err(Error::Invalid("command response"));
        };
        if response.request_id != request_id || response.method != "command/submit" {
            return Err(Error::Invalid("command response"));
        }
        if !response.ok {
            return Err(Error::Invalid("command rejected"));
        }
        let value = response.result.ok_or(Error::Invalid("command result"))?;
        let result: protocol::CommandSubmitResult =
            serde_json::from_value(value).map_err(|_| Error::Invalid("command result"))?;
        Ok(CommandReceipt {
            status: result.status,
            receipt_id: result.receipt_id,
            revision: result.revision,
        })
    }
    pub async fn status(&self, capabilities: CapabilitySet) -> Result<StatusProjection> {
        capabilities.validate()?;
        capabilities.require(Capability::StatusRead)?;
        let value = self.transport.status(capabilities).await?;
        validate_status(&value)?;
        Ok(value)
    }
    pub fn events<'a>(&'a self, query: EventQuery) -> Result<EventIterator<'a, T>> {
        query.validate()?;
        Ok(EventIterator {
            client: self,
            query,
            cursor: None,
            page: None,
            index: 0,
            done: false,
        })
    }
    pub fn initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }
}

/// Async page-by-page event iteration.  The iterator never asks for a page
/// larger than [`MAX_EVENT_PAGE_SIZE`] and stops on a repeated cursor.
pub struct EventIterator<'a, T> {
    client: &'a XcbClient<T>,
    query: EventQuery,
    cursor: Option<String>,
    page: Option<EventPage>,
    index: usize,
    done: bool,
}
impl<'a, T: ProtocolTransport> EventIterator<'a, T> {
    pub async fn next_event(&mut self) -> Result<Option<EventProjection>> {
        if self.done {
            return Ok(None);
        }
        loop {
            if let Some(page) = &self.page
                && let Some(event) = page.events.get(self.index)
            {
                self.index += 1;
                return Ok(Some(event.clone()));
            }
            let previous_cursor = self.cursor.clone();
            let mut query = self.query.clone();
            query.cursor = self.cursor.clone();
            let page = self.client.transport.events(query).await?;
            validate_event_page(&page)?;
            self.page = Some(page.clone());
            self.index = 0;
            if page.events.is_empty() {
                self.done = true;
                return Ok(None);
            }
            if page.has_more {
                let next = page
                    .next_cursor
                    .clone()
                    .ok_or(Error::Invalid("event cursor"))?;
                if Some(next.clone()) == previous_cursor {
                    return Err(Error::Invalid("event cursor did not advance"));
                }
                self.cursor = Some(next);
            } else {
                self.done = true;
            }
        }
    }
    pub async fn collect(mut self, maximum: usize) -> Result<Vec<EventProjection>> {
        if maximum == 0 || maximum > projections::MAX_REDACTIONS * MAX_EVENT_PAGE_SIZE {
            return Err(Error::Limit("event collection"));
        }
        let mut result = Vec::new();
        while let Some(event) = self.next_event().await? {
            result.push(event);
            if result.len() >= maximum {
                break;
            }
        }
        Ok(result)
    }
}

#[derive(Debug, Clone)]
struct ReferenceTask {
    id: String,
    revision: u64,
    state: String,
    title: String,
    workspace: Option<String>,
    prompt: String,
    provider: Option<String>,
    model: Option<String>,
    detail: Option<String>,
}
#[derive(Debug, Clone)]
struct ReferenceEvent {
    sequence: u64,
    id: String,
    revision: u64,
    entity_id: String,
    kind: String,
    data: Value,
}
#[derive(Debug, Clone)]
struct ReferenceReceipt {
    fingerprint: String,
    receipt_id: String,
    revision: String,
}
#[derive(Debug, Default)]
struct ReferenceState {
    revision: u64,
    tasks: BTreeMap<String, ReferenceTask>,
    events: Vec<ReferenceEvent>,
    receipts: BTreeMap<String, ReferenceReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceFactoryOptions {
    pub id: String,
    pub initial_capabilities: CapabilitySet,
}
impl Default for ReferenceFactoryOptions {
    fn default() -> Self {
        Self {
            id: "factory_ref".into(),
            initial_capabilities: CapabilitySet::public(),
        }
    }
}

#[derive(Clone)]
pub struct ReferenceTransport {
    state: Arc<Mutex<ReferenceState>>,
    factory_id: String,
    initial_capabilities: CapabilitySet,
}
impl std::fmt::Debug for ReferenceTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReferenceTransport")
            .field("factory_id", &self.factory_id)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct ReferenceFactory {
    transport: ReferenceTransport,
}
impl std::fmt::Debug for ReferenceFactory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReferenceFactory")
            .field("factory_id", &self.transport.factory_id)
            .finish_non_exhaustive()
    }
}
impl ReferenceFactory {
    pub fn new() -> Self {
        Self::with_options(ReferenceFactoryOptions::default()).expect("static reference options")
    }
    pub fn with_options(options: ReferenceFactoryOptions) -> Result<Self> {
        projections::validate_id(&options.id)?;
        options.initial_capabilities.validate()?;
        Ok(Self {
            transport: ReferenceTransport {
                state: Arc::new(Mutex::new(ReferenceState::default())),
                factory_id: options.id,
                initial_capabilities: options.initial_capabilities,
            },
        })
    }
    pub fn transport(&self) -> ReferenceTransport {
        self.transport.clone()
    }
    pub fn client(&self) -> Result<XcbClient<ReferenceTransport>> {
        XcbClient::new(self.transport(), ClientOptions::default())
    }
    pub fn client_with_options(
        &self,
        options: ClientOptions,
    ) -> Result<XcbClient<ReferenceTransport>> {
        XcbClient::new(self.transport(), options)
    }
    pub fn status(&self, capabilities: CapabilitySet) -> Result<StatusProjection> {
        self.transport.status_now(capabilities)
    }
    pub fn events(&self, query: EventQuery) -> Result<EventPage> {
        self.transport.events_now(query)
    }
}
impl Default for ReferenceFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl ReferenceTransport {
    fn revision_string(revision: u64) -> String {
        format!("rev_{revision}")
    }
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, ReferenceState>> {
        self.state
            .lock()
            .map_err(|_| Error::Invalid("reference state lock"))
    }
    fn status_now(&self, capabilities: CapabilitySet) -> Result<StatusProjection> {
        capabilities.validate()?;
        capabilities.require(Capability::StatusRead)?;
        let state = self.lock()?;
        let tasks: Vec<_> = state
            .tasks
            .values()
            .map(|task| TaskProjection {
                id: task.id.clone(),
                revision: task.revision,
                state: task.state.clone(),
                title: Some(task.title.clone()),
                workspace: task.workspace.clone(),
                prompt: Some(task.prompt.clone()),
                provider: task.provider.clone(),
                model: task.model.clone(),
                detail: task.detail.clone(),
            })
            .collect();
        let base = StatusProjection {
            schema: STATUS_PROJECTION_SCHEMA.into(),
            schema_digest: schema_digest(),
            revision: state.revision,
            factory: FactoryProjection {
                id: self.factory_id.clone(),
                state: "ready".into(),
                revision: state.revision,
                capabilities: self.initial_capabilities.names(),
            },
            tasks,
            redactions: Vec::new(),
        };
        redact_status(&base, &capabilities)
    }
    fn events_now(&self, query: EventQuery) -> Result<EventPage> {
        query.validate()?;
        let after = query
            .cursor
            .as_deref()
            .map(projections::decode_cursor)
            .transpose()?
            .unwrap_or(0);
        let state = self.lock()?;
        let mut result = Vec::new();
        for event in state.events.iter().filter(|event| {
            event.sequence > after
                && query
                    .entity_id
                    .as_deref()
                    .is_none_or(|entity| entity == event.entity_id)
        }) {
            if result.len() > query.limit {
                break;
            }
            let projection = EventProjection {
                schema: EVENT_PROJECTION_SCHEMA.into(),
                schema_digest: schema_digest(),
                sequence: event.sequence,
                id: event.id.clone(),
                revision: event.revision,
                entity_id: event.entity_id.clone(),
                kind: event.kind.clone(),
                data: event.data.clone(),
                redacted: false,
            };
            result.push(redact_event(&projection, &query.capabilities)?);
        }
        let has_more = result.len() > query.limit;
        if has_more {
            result.truncate(query.limit);
        }
        let next_cursor = has_more
            .then(|| {
                result
                    .last()
                    .map(|event| projections::encode_cursor(event.sequence))
            })
            .flatten()
            .transpose()?;
        let page = EventPage {
            schema: EVENT_PROJECTION_SCHEMA.into(),
            schema_digest: schema_digest(),
            events: result,
            next_cursor,
            has_more,
        };
        validate_event_page(&page)?;
        Ok(page)
    }
    fn round_trip_now(&self, frame: Vec<u8>) -> Result<Vec<u8>> {
        let frame = protocol::decode_frame(&frame)?;
        let protocol::Frame::Request(request) = frame else {
            return Err(Error::Invalid("reference request"));
        };
        match request.method.as_str() {
            "initialize" => {
                let capabilities = request
                    .capabilities
                    .ok_or(Error::Invalid("initialize capabilities"))?;
                let response = protocol::initialize_response(request.request_id, capabilities)?;
                protocol::encode_frame(&response)
            }
            "command/submit" => {
                let params: protocol::CommandSubmitParams = serde_json::from_value(request.params)
                    .map_err(|_| Error::Invalid("command params"))?;
                let idempotency = request
                    .idempotency_key
                    .ok_or(Error::Invalid("idempotency key"))?;
                let fingerprint = protocol::canonical_json(&json!({
                    "command": params.command,
                    "arguments": params.arguments,
                    "expectedRevision": request.expected_revision,
                }))?;
                let mut state = self.lock()?;
                if let Some(previous) = state.receipts.get(&idempotency) {
                    if previous.fingerprint != fingerprint {
                        return self.error_bytes(
                            request.request_id,
                            "command/submit",
                            protocol::ErrorCode::IdempotencyConflict,
                            "idempotency key was reused",
                            false,
                        );
                    }
                    let response = protocol::command_submit_response(
                        request.request_id,
                        "replayed",
                        previous.receipt_id.clone(),
                        Some(previous.revision.clone()),
                    )?;
                    return protocol::encode_frame(&response);
                }
                let current = Self::revision_string(state.revision);
                if request
                    .expected_revision
                    .as_deref()
                    .is_some_and(|expected| expected != current)
                {
                    return self.error_bytes(
                        request.request_id,
                        "command/submit",
                        protocol::ErrorCode::RevisionConflict,
                        "expected revision is stale",
                        true,
                    );
                }
                if matches!(params.command.as_str(), "task/run" | "task/cancel") {
                    apply_task_command(&mut state, &params.command, &params.arguments)?;
                }
                state.revision = state.revision.saturating_add(1);
                let revision = Self::revision_string(state.revision);
                let receipt_id = format!("rcpt_ref_{}", state.revision);
                state.receipts.insert(
                    idempotency,
                    ReferenceReceipt {
                        fingerprint,
                        receipt_id: receipt_id.clone(),
                        revision: revision.clone(),
                    },
                );
                let sequence = state.events.len() as u64 + 1;
                let revision_number = state.revision;
                state.events.push(ReferenceEvent {
                    sequence,
                    id: format!("evt_ref_{sequence}"),
                    revision: revision_number,
                    entity_id: self.factory_id.clone(),
                    kind: format!("command_{}", params.command.replace('/', "_")),
                    data: json!({
                        "command": params.command,
                        "arguments": params.arguments,
                    }),
                });
                let response = protocol::command_submit_response(
                    request.request_id,
                    "accepted",
                    receipt_id,
                    Some(revision),
                )?;
                protocol::encode_frame(&response)
            }
            _ => self.error_bytes(
                request.request_id,
                request.method,
                protocol::ErrorCode::InvalidRequest,
                "unsupported method",
                false,
            ),
        }
    }
    fn error_bytes(
        &self,
        request_id: String,
        method: impl Into<String>,
        code: protocol::ErrorCode,
        message: &'static str,
        retryable: bool,
    ) -> Result<Vec<u8>> {
        let response = protocol::error_response(
            request_id,
            method,
            protocol::ProtocolError {
                code,
                message: message.into(),
                retryable,
            },
        )?;
        protocol::encode_frame(&response)
    }
}

fn apply_task_command(state: &mut ReferenceState, command: &str, arguments: &Value) -> Result<()> {
    let object = arguments
        .as_object()
        .ok_or(Error::Invalid("command arguments"))?;
    let task_id = object
        .get("taskId")
        .and_then(Value::as_str)
        .ok_or(Error::Invalid("task id"))?;
    projections::validate_id(task_id)?;
    match command {
        "task/run" => {
            let prompt = object
                .get("prompt")
                .and_then(Value::as_str)
                .ok_or(Error::Invalid("task prompt"))?;
            if prompt.is_empty() || prompt.len() > 16 * 1024 || prompt.chars().any(char::is_control)
            {
                return Err(Error::Invalid("task prompt"));
            }
            let task = ReferenceTask {
                id: task_id.into(),
                revision: state.revision.saturating_add(1),
                state: "queued".into(),
                title: prompt.chars().take(96).collect(),
                workspace: object
                    .get("workspace")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                prompt: prompt.into(),
                provider: object
                    .get("provider")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                model: object
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                detail: None,
            };
            state.tasks.insert(task.id.clone(), task);
        }
        "task/cancel" => {
            let task = state
                .tasks
                .get_mut(task_id)
                .ok_or(Error::Invalid("task not found"))?;
            task.state = "cancelled".into();
            task.revision = state.revision.saturating_add(1);
            task.detail = Some("cancelled by client".into());
        }
        _ => (),
    }
    Ok(())
}

impl ProtocolTransport for ReferenceTransport {
    fn round_trip<'a>(&'a self, frame: Vec<u8>) -> BoxFuture<'a, Vec<u8>> {
        Box::pin(async move { self.round_trip_now(frame) })
    }
    fn status<'a>(&'a self, capabilities: CapabilitySet) -> BoxFuture<'a, StatusProjection> {
        Box::pin(async move { self.status_now(capabilities) })
    }
    fn events<'a>(&'a self, query: EventQuery) -> BoxFuture<'a, EventPage> {
        Box::pin(async move { self.events_now(query) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run<'a, F: Future<Output = Result<T>> + 'a, T>(future: F) -> T {
        // The reference client futures do not yield.  A tiny local executor
        // keeps the core crate runtime-free while still exercising async APIs.
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        let mut future = Box::pin(future);
        loop {
            match Future::poll(future.as_mut(), &mut context) {
                std::task::Poll::Ready(value) => return value.expect("future"),
                std::task::Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    #[test]
    fn reference_factory_round_trips_typed_commands_and_replays() {
        let factory = ReferenceFactory::new();
        let client = factory.client().expect("client");
        let init = run(client.initialize());
        assert_eq!(init.protocol_version, protocol::SCHEMA);
        let mut command = task_run(TaskRunInput {
            task_id: "task_ref".into(),
            prompt: "inspect the fixture".into(),
            workspace: Some("fixture".into()),
            provider: Some("claude".into()),
            model: None,
        })
        .expect("command");
        command.idempotency_key = Some("idem_ref".into());
        let receipt = run(client.submit(command.clone()));
        assert_eq!(receipt.status, "accepted");
        let replay = run(client.submit(command));
        assert_eq!(replay.status, "replayed");
        assert_eq!(replay.receipt_id, receipt.receipt_id);
        let status = run(client.status(CapabilitySet::public()));
        assert_eq!(status.tasks.len(), 1);
        assert!(status.tasks[0].prompt.is_none());
        let private = run(client.status(CapabilitySet::all()));
        assert_eq!(
            private.tasks[0].prompt.as_deref(),
            Some("inspect the fixture")
        );
    }

    #[test]
    fn event_iterator_reads_bounded_pages_and_redacts_arguments() {
        let factory = ReferenceFactory::new();
        let client = factory.client().expect("client");
        let _ = run(client.initialize());
        for index in 0..3 {
            let mut command = task_run(TaskRunInput {
                task_id: format!("task_{index}"),
                prompt: format!("prompt {index}"),
                workspace: None,
                provider: None,
                model: None,
            })
            .expect("command");
            command.idempotency_key = Some(format!("idem_{index}"));
            let _ = run(client.submit(command));
        }
        let query = EventQuery {
            limit: 2,
            ..EventQuery::default()
        };
        let iterator = client.events(query).expect("events");
        let events = run(iterator.collect(8));
        assert_eq!(events.len(), 3);
        assert!(events.iter().all(|event| event.redacted));
        assert!(events.iter().all(|event| event.data == json!({})));
    }

    #[test]
    fn schema_digest_options_fail_closed() {
        let factory = ReferenceFactory::new();
        let options = ClientOptions {
            expected_schema_digest: "0".repeat(64),
            ..Default::default()
        };
        assert!(factory.client_with_options(options).is_err());
        assert!(events_read(&EventQuery::default()).is_ok());
    }
}
