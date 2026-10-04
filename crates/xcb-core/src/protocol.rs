//! The first transport-neutral xcb protocol slice.
//!
//! Frames are canonical JSON followed by one LF. This module owns no socket,
//! stdio, relay, or Valhalla transport; it only validates the versioned
//! initialize and command/submit messages that a transport may carry.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, Result};

pub const SCHEMA: &str = "xcb.protocol.v1";
pub const VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 64 * 1024;
pub const MAX_ARGUMENT_BYTES: usize = 16 * 1024;
pub const MAX_ERROR_BYTES: usize = 512;
pub const MAX_CAPABILITIES: usize = 32;
pub const MAX_JSON_DEPTH: usize = 12;
pub const FEATURES: [&str; 2] = ["command.submit", "receipt.reference"];

const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
const MIN_SAFE_INTEGER: i64 = -9_007_199_254_740_991;
const REQUEST_KEYS: [&str; 8] = [
    "schema",
    "kind",
    "requestId",
    "method",
    "expectedRevision",
    "idempotencyKey",
    "capabilities",
    "params",
];
const RESPONSE_KEYS: [&str; 7] = [
    "schema",
    "kind",
    "requestId",
    "method",
    "ok",
    "result",
    "error",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    UnsupportedVersion,
    UnsupportedCapability,
    RevisionConflict,
    IdempotencyConflict,
    NotFound,
    Busy,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub versions: Vec<String>,
    pub features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct InitializeParams {
    pub client_name: String,
    pub client_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CommandSubmitParams {
    pub command: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: String,
    pub capabilities: Capabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CommandSubmitResult {
    pub status: String,
    pub receipt_id: String,
    pub revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RequestFrame {
    pub schema: String,
    pub kind: String,
    pub request_id: String,
    pub method: String,
    pub expected_revision: Option<String>,
    pub idempotency_key: Option<String>,
    pub capabilities: Option<Capabilities>,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ResponseFrame {
    pub schema: String,
    pub kind: String,
    pub request_id: String,
    pub method: String,
    pub ok: bool,
    pub result: Option<Value>,
    pub error: Option<ProtocolError>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Request(RequestFrame),
    Response(ResponseFrame),
}

fn invalid(what: &'static str) -> Error {
    Error::Invalid(what)
}

fn text(value: &str, max: usize, what: &'static str, allow_empty: bool) -> Result<()> {
    if (!allow_empty && value.is_empty())
        || value.len() > max
        || value.chars().any(char::is_control)
    {
        return Err(invalid(what));
    }
    Ok(())
}

fn token(value: &str, prefix: &str, max: usize, what: &'static str) -> Result<()> {
    text(value, max, what, false)?;
    if value.len() <= prefix.len()
        || !value.starts_with(prefix)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
    {
        return Err(invalid(what));
    }
    Ok(())
}

fn revision(value: Option<&String>) -> Result<()> {
    if let Some(value) = value {
        token(value, "", 160, "protocol revision")?;
    }
    Ok(())
}

fn sorted_unique(values: &[String], what: &'static str) -> Result<()> {
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(invalid(what));
    }
    Ok(())
}

pub fn validate_capabilities(value: &Capabilities) -> Result<()> {
    if value.versions.is_empty()
        || value.versions.len() > 8
        || value.features.len() > MAX_CAPABILITIES
    {
        return Err(invalid("protocol capabilities"));
    }
    value
        .versions
        .iter()
        .try_for_each(|version| text(version, 96, "protocol version", false))?;
    value
        .features
        .iter()
        .try_for_each(|feature| text(feature, 96, "protocol capability", false))?;
    sorted_unique(&value.versions, "protocol capability order")?;
    sorted_unique(&value.features, "protocol capability order")?;
    if !value.versions.iter().any(|version| version == SCHEMA) {
        return Err(invalid("protocol version"));
    }
    if value
        .features
        .iter()
        .any(|feature| !FEATURES.contains(&feature.as_str()))
    {
        return Err(invalid("protocol capability"));
    }
    Ok(())
}

pub fn negotiate_capabilities(offered: &Capabilities) -> Result<Capabilities> {
    validate_capabilities(offered)?;
    Ok(Capabilities {
        versions: vec![SCHEMA.to_owned()],
        features: FEATURES
            .iter()
            .filter(|feature| {
                offered
                    .features
                    .iter()
                    .any(|candidate| candidate == *feature)
            })
            .map(|feature| (*feature).to_owned())
            .collect(),
    })
}

fn validate_json(value: &Value, depth: usize) -> Result<()> {
    if depth > MAX_JSON_DEPTH {
        return Err(invalid("protocol argument depth"));
    }
    match value {
        Value::Null | Value::Bool(_) => Ok(()),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                if !(MIN_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&value) {
                    return Err(invalid("protocol argument number"));
                }
            } else if number
                .as_u64()
                .is_none_or(|value| value > MAX_SAFE_INTEGER as u64)
            {
                return Err(invalid("protocol argument number"));
            }
            Ok(())
        }
        Value::String(_) => Ok(()),
        Value::Array(items) => {
            if items.len() > 128 {
                return Err(invalid("protocol argument array"));
            }
            items
                .iter()
                .try_for_each(|item| validate_json(item, depth + 1))
        }
        Value::Object(map) => {
            if map.len() > 128 {
                return Err(invalid("protocol argument object"));
            }
            for (key, item) in map {
                text(key, 128, "protocol argument key", true)?;
                validate_json(item, depth + 1)?;
            }
            Ok(())
        }
    }
}

fn validate_arguments(value: &Value) -> Result<()> {
    if !value.is_object() {
        return Err(invalid("protocol arguments"));
    }
    validate_json(value, 0)?;
    if canonical_json(value)?.len() > MAX_ARGUMENT_BYTES {
        return Err(invalid("protocol argument limit"));
    }
    Ok(())
}

fn validate_params(request: &RequestFrame) -> Result<()> {
    match request.method.as_str() {
        "initialize" => {
            if request.expected_revision.is_some()
                || request.idempotency_key.is_some()
                || request.capabilities.is_none()
            {
                return Err(invalid("initialize metadata"));
            }
            let params: InitializeParams = serde_json::from_value(request.params.clone())
                .map_err(|_| invalid("initialize params"))?;
            text(&params.client_name, 128, "client name", false)?;
            text(&params.client_version, 64, "client version", false)?;
            validate_capabilities(request.capabilities.as_ref().expect("checked above"))
        }
        "command/submit" => {
            if request.idempotency_key.is_none() || request.capabilities.is_some() {
                return Err(invalid("command metadata"));
            }
            token(
                request.idempotency_key.as_ref().expect("checked above"),
                "idem_",
                160,
                "protocol idempotency key",
            )?;
            let params: CommandSubmitParams = serde_json::from_value(request.params.clone())
                .map_err(|_| invalid("command params"))?;
            text(&params.command, 96, "protocol command", false)?;
            if !params
                .command
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._/-".contains(&byte))
                || !params.command.as_bytes()[0].is_ascii_alphabetic()
            {
                return Err(invalid("protocol command"));
            }
            validate_arguments(&params.arguments)
        }
        _ => Err(invalid("protocol method")),
    }
}

pub fn validate_error(value: &ProtocolError) -> Result<()> {
    text(
        &value.message,
        MAX_ERROR_BYTES,
        "protocol error message",
        false,
    )?;
    Ok(())
}

fn validate_result(response: &ResponseFrame) -> Result<()> {
    let value = response
        .result
        .as_ref()
        .ok_or_else(|| invalid("protocol response result"))?;
    match response.method.as_str() {
        "initialize" => {
            let result: InitializeResult =
                serde_json::from_value(value.clone()).map_err(|_| invalid("initialize result"))?;
            if result.protocol_version != SCHEMA {
                return Err(invalid("protocol response version"));
            }
            validate_capabilities(&result.capabilities)
        }
        "command/submit" => {
            let result: CommandSubmitResult =
                serde_json::from_value(value.clone()).map_err(|_| invalid("command result"))?;
            if result.status != "accepted" && result.status != "replayed" {
                return Err(invalid("protocol command status"));
            }
            token(&result.receipt_id, "rcpt_", 160, "protocol receipt id")?;
            revision(result.revision.as_ref())
        }
        _ => Err(invalid("protocol method")),
    }
}

pub fn validate_frame(frame: &Frame) -> Result<()> {
    match frame {
        Frame::Request(request) => {
            if request.schema != SCHEMA || request.kind != "request" {
                return Err(invalid("protocol request envelope"));
            }
            token(&request.request_id, "req_", 128, "protocol request id")?;
            revision(request.expected_revision.as_ref())?;
            if let Some(key) = request.idempotency_key.as_ref() {
                token(key, "idem_", 160, "protocol idempotency key")?;
            }
            validate_params(request)
        }
        Frame::Response(response) => {
            if response.schema != SCHEMA || response.kind != "response" {
                return Err(invalid("protocol response envelope"));
            }
            token(&response.request_id, "req_", 128, "protocol request id")?;
            if response.method != "initialize" && response.method != "command/submit" {
                return Err(invalid("protocol method"));
            }
            if response.ok {
                if response.error.is_some() {
                    return Err(invalid("protocol success error"));
                }
                validate_result(response)
            } else {
                if response.result.is_some() {
                    return Err(invalid("protocol error result"));
                }
                let error = response
                    .error
                    .as_ref()
                    .ok_or_else(|| invalid("protocol error"))?;
                validate_error(error)
            }
        }
    }
}

fn require_keys(value: &Value, expected: &[&str]) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("protocol frame shape"))?;
    if object.len() != expected.len() || expected.iter().any(|key| !object.contains_key(*key)) {
        return Err(invalid("protocol frame fields"));
    }
    Ok(())
}

/// Escape a string exactly as `JSON.stringify`: named controls use short
/// escapes, other controls use lower-case `\\u00xx`, and non-ASCII is raw.
fn escape_string(value: &str, output: &mut String) {
    output.push('"');
    for ch in value.chars() {
        match ch {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{0008}' => output.push_str("\\b"),
            '\u{0009}' => output.push_str("\\t"),
            '\u{000a}' => output.push_str("\\n"),
            '\u{000c}' => output.push_str("\\f"),
            '\u{000d}' => output.push_str("\\r"),
            ch if ch < '\u{0020}' => {
                output.push_str("\\u00");
                output.push(char::from_digit((ch as u32) >> 4, 16).unwrap_or('0'));
                output.push(char::from_digit((ch as u32) & 0xf, 16).unwrap_or('0'));
            }
            ch => output.push(ch),
        }
    }
    output.push('"');
}

fn write_canonical(value: &Value, output: &mut String) -> Result<()> {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                output.push_str(&value.to_string());
            } else if let Some(value) = number.as_u64() {
                output.push_str(&value.to_string());
            } else {
                return Err(invalid("canonical JSON carries integers only"));
            }
        }
        Value::String(value) => escape_string(value, output),
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_canonical(value, output)?;
            }
            output.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
            output.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                escape_string(key, output);
                output.push(':');
                write_canonical(&map[*key], output)?;
            }
            output.push('}');
        }
    }
    Ok(())
}

/// Canonical JSON with safe integer-only numbers and JavaScript-compatible key order.
pub fn canonical_json(value: &Value) -> Result<String> {
    let mut output = String::with_capacity(256);
    write_canonical(value, &mut output)?;
    Ok(output)
}

fn frame_value(frame: &Frame) -> Result<Value> {
    match frame {
        Frame::Request(request) => serde_json::to_value(request),
        Frame::Response(response) => serde_json::to_value(response),
    }
    .map_err(|_| invalid("protocol frame serialization"))
}

/// Encode one validated frame as canonical JSON plus one LF.
pub fn encode_frame(frame: &Frame) -> Result<Vec<u8>> {
    validate_frame(frame)?;
    let value = frame_value(frame)?;
    let mut bytes = canonical_json(&value)?.into_bytes();
    bytes.push(b'\n');
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(invalid("protocol frame limit"));
    }
    Ok(bytes)
}

/// Decode exactly one canonical frame. A transport may split or join bytes,
/// but this function accepts only a complete frame and never performs I/O.
pub fn decode_frame(bytes: &[u8]) -> Result<Frame> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME_BYTES || bytes.last() != Some(&b'\n') {
        return Err(invalid("protocol frame boundary"));
    }
    if bytes[..bytes.len() - 1]
        .iter()
        .any(|byte| *byte == b'\n' || *byte == b'\r')
    {
        return Err(invalid("protocol frame boundary"));
    }
    let body = std::str::from_utf8(&bytes[..bytes.len() - 1])
        .map_err(|_| invalid("protocol frame encoding"))?;
    if body.is_empty() {
        return Err(invalid("protocol frame JSON"));
    }
    let value: Value = serde_json::from_str(body).map_err(|_| invalid("protocol frame JSON"))?;
    if canonical_json(&value).map_err(|_| invalid("protocol frame JSON"))? != body {
        return Err(invalid("protocol frame is not canonical"));
    }
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("protocol frame shape"))?;
    let frame = match kind {
        "request" => {
            require_keys(&value, &REQUEST_KEYS)?;
            Frame::Request(
                serde_json::from_value(value).map_err(|_| invalid("protocol request fields"))?,
            )
        }
        "response" => {
            require_keys(&value, &RESPONSE_KEYS)?;
            Frame::Response(
                serde_json::from_value(value).map_err(|_| invalid("protocol response fields"))?,
            )
        }
        _ => return Err(invalid("protocol frame kind")),
    };
    validate_frame(&frame)?;
    Ok(frame)
}

pub fn initialize_request(
    request_id: impl Into<String>,
    client_name: impl Into<String>,
    client_version: impl Into<String>,
    capabilities: Capabilities,
) -> Result<Frame> {
    let params = InitializeParams {
        client_name: client_name.into(),
        client_version: client_version.into(),
    };
    let frame = Frame::Request(RequestFrame {
        schema: SCHEMA.to_owned(),
        kind: "request".to_owned(),
        request_id: request_id.into(),
        method: "initialize".to_owned(),
        expected_revision: None,
        idempotency_key: None,
        capabilities: Some(capabilities),
        params: serde_json::to_value(params).map_err(|_| invalid("initialize params"))?,
    });
    validate_frame(&frame)?;
    Ok(frame)
}

pub fn command_submit_request(
    request_id: impl Into<String>,
    command: impl Into<String>,
    arguments: Value,
    expected_revision: Option<String>,
    idempotency_key: impl Into<String>,
) -> Result<Frame> {
    let params = CommandSubmitParams {
        command: command.into(),
        arguments,
    };
    let frame = Frame::Request(RequestFrame {
        schema: SCHEMA.to_owned(),
        kind: "request".to_owned(),
        request_id: request_id.into(),
        method: "command/submit".to_owned(),
        expected_revision,
        idempotency_key: Some(idempotency_key.into()),
        capabilities: None,
        params: serde_json::to_value(params).map_err(|_| invalid("command params"))?,
    });
    validate_frame(&frame)?;
    Ok(frame)
}

pub fn initialize_response(
    request_id: impl Into<String>,
    capabilities: Capabilities,
) -> Result<Frame> {
    let result = InitializeResult {
        protocol_version: SCHEMA.to_owned(),
        capabilities: negotiate_capabilities(&capabilities)?,
    };
    let frame = Frame::Response(ResponseFrame {
        schema: SCHEMA.to_owned(),
        kind: "response".to_owned(),
        request_id: request_id.into(),
        method: "initialize".to_owned(),
        ok: true,
        result: Some(serde_json::to_value(result).map_err(|_| invalid("initialize result"))?),
        error: None,
    });
    validate_frame(&frame)?;
    Ok(frame)
}

pub fn command_submit_response(
    request_id: impl Into<String>,
    status: impl Into<String>,
    receipt_id: impl Into<String>,
    revision: Option<String>,
) -> Result<Frame> {
    let result = CommandSubmitResult {
        status: status.into(),
        receipt_id: receipt_id.into(),
        revision,
    };
    let frame = Frame::Response(ResponseFrame {
        schema: SCHEMA.to_owned(),
        kind: "response".to_owned(),
        request_id: request_id.into(),
        method: "command/submit".to_owned(),
        ok: true,
        result: Some(serde_json::to_value(result).map_err(|_| invalid("command result"))?),
        error: None,
    });
    validate_frame(&frame)?;
    Ok(frame)
}

pub fn error_response(
    request_id: impl Into<String>,
    method: impl Into<String>,
    error: ProtocolError,
) -> Result<Frame> {
    let frame = Frame::Response(ResponseFrame {
        schema: SCHEMA.to_owned(),
        kind: "response".to_owned(),
        request_id: request_id.into(),
        method: method.into(),
        ok: false,
        result: None,
        error: Some(error),
    });
    validate_frame(&frame)?;
    Ok(frame)
}
