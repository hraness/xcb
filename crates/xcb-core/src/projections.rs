//! Portable, capability-aware projections shared by the native and
//! compatibility clients.
//!
//! A projection is presentation data, never authority.  It deliberately
//! carries the protocol schema digest so a consumer cannot silently interpret
//! a newer wire shape as the old one.  Secret-bearing fields are not present
//! in the public shape; the optional capabilities below only reveal bounded
//! diagnostic fields that the host explicitly grants.

use crate::{Error, Result, hash::sha256_hex, protocol};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

pub const STATUS_PROJECTION_SCHEMA: &str = "xcb.projection.status.v1";
pub const EVENT_PROJECTION_SCHEMA: &str = "xcb.projection.event.v1";
pub const MAX_STATUS_PROJECTION_BYTES: usize = 64 * 1024;
pub const MAX_EVENT_PAGE_BYTES: usize = 64 * 1024;
pub const MAX_EVENT_PAGE_SIZE: usize = 128;
pub const MAX_TASKS: usize = 256;
pub const MAX_REDACTIONS: usize = 64;

/// The schema descriptor is intentionally small and transport independent.
/// TypeScript uses this exact UTF-8 string when computing the same digest.
pub const SCHEMA_DIGEST_INPUT: &str = "{\"schema\":\"xcb.protocol.v1\",\"version\":1,\"features\":[\"command.submit\",\"receipt.reference\"],\"methods\":[\"initialize\",\"command/submit\"]}";

/// SHA-256 of the frozen protocol descriptor.  Computing it rather than
/// copying a generated value keeps source builds and the TypeScript SDK in
/// lockstep while still making every projection check the digest.
pub fn schema_digest() -> String {
    sha256_hex(SCHEMA_DIGEST_INPUT.as_bytes())
}

pub fn check_schema_digest(value: &str) -> Result<()> {
    if value != schema_digest() {
        return Err(Error::Invalid("schema digest"));
    }
    Ok(())
}

pub fn check_schema(schema: &str, digest: &str, expected: &str) -> Result<()> {
    if schema != expected {
        return Err(Error::Invalid("projection schema"));
    }
    check_schema_digest(digest)
}

/// Capabilities that may be requested by an agent.  `status.read` and
/// `events.read` are the public, non-secret defaults; every other capability
/// requires an explicit host grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    StatusRead,
    EventsRead,
    TaskRead,
    WorkspaceRead,
    AccountRead,
    CommandInspect,
    ReceiptRead,
}
impl Capability {
    pub const ALL: [Self; 7] = [
        Self::StatusRead,
        Self::EventsRead,
        Self::TaskRead,
        Self::WorkspaceRead,
        Self::AccountRead,
        Self::CommandInspect,
        Self::ReceiptRead,
    ];
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StatusRead => "status-read",
            Self::EventsRead => "events-read",
            Self::TaskRead => "task-read",
            Self::WorkspaceRead => "workspace-read",
            Self::AccountRead => "account-read",
            Self::CommandInspect => "command-inspect",
            Self::ReceiptRead => "receipt-read",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CapabilitySet(Vec<Capability>);
impl CapabilitySet {
    pub fn empty() -> Self {
        Self(Vec::new())
    }
    pub fn public() -> Self {
        Self(vec![Capability::StatusRead, Capability::EventsRead])
    }
    pub fn all() -> Self {
        Self(Capability::ALL.to_vec())
    }
    pub fn from_iter(values: impl IntoIterator<Item = Capability>) -> Result<Self> {
        let mut set = BTreeSet::new();
        for value in values {
            set.insert(value);
        }
        let values: Vec<_> = set.into_iter().collect();
        if values.len() > Capability::ALL.len() {
            return Err(Error::Limit("capabilities"));
        }
        Ok(Self(values))
    }
    pub fn validate(&self) -> Result<()> {
        if self.0.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(Error::Invalid("capability order"));
        }
        Ok(())
    }
    pub fn contains(&self, capability: Capability) -> bool {
        self.0.binary_search(&capability).is_ok()
    }
    pub fn require(&self, capability: Capability) -> Result<()> {
        if self.contains(capability) {
            Ok(())
        } else {
            Err(Error::Invalid("capability"))
        }
    }
    pub fn as_slice(&self) -> &[Capability] {
        &self.0
    }
    pub fn names(&self) -> Vec<String> {
        self.0
            .iter()
            .map(|capability| capability.as_str().to_owned())
            .collect()
    }
}
impl Default for CapabilitySet {
    fn default() -> Self {
        Self::public()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FactoryProjection {
    pub id: String,
    pub state: String,
    pub revision: u64,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskProjection {
    pub id: String,
    pub revision: u64,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StatusProjection {
    pub schema: String,
    pub schema_digest: String,
    pub revision: u64,
    pub factory: FactoryProjection,
    pub tasks: Vec<TaskProjection>,
    pub redactions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventProjection {
    pub schema: String,
    pub schema_digest: String,
    pub sequence: u64,
    pub id: String,
    pub revision: u64,
    pub entity_id: String,
    pub kind: String,
    pub data: Value,
    pub redacted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventPage {
    pub schema: String,
    pub schema_digest: String,
    pub events: Vec<EventProjection>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventQuery {
    pub cursor: Option<String>,
    pub limit: usize,
    pub entity_id: Option<String>,
    pub capabilities: CapabilitySet,
}
impl Default for EventQuery {
    fn default() -> Self {
        Self {
            cursor: None,
            limit: MAX_EVENT_PAGE_SIZE,
            entity_id: None,
            capabilities: CapabilitySet::public(),
        }
    }
}
impl EventQuery {
    pub fn validate(&self) -> Result<()> {
        self.capabilities.validate()?;
        self.capabilities.require(Capability::EventsRead)?;
        if !(1..=MAX_EVENT_PAGE_SIZE).contains(&self.limit) {
            return Err(Error::Limit("event page"));
        }
        if let Some(cursor) = self.cursor.as_deref() {
            decode_cursor(cursor)?;
        }
        if let Some(entity) = self.entity_id.as_deref() {
            validate_id(entity)?;
        }
        Ok(())
    }
}

pub fn encode_cursor(sequence: u64) -> Result<String> {
    if sequence == 0 {
        return Err(Error::Invalid("event cursor"));
    }
    Ok(format!("e:{sequence}"))
}
pub fn decode_cursor(value: &str) -> Result<u64> {
    let sequence = value
        .strip_prefix("e:")
        .ok_or(Error::Invalid("event cursor"))?
        .parse::<u64>()
        .map_err(|_| Error::Invalid("event cursor"))?;
    if sequence == 0 || value.len() > 32 {
        return Err(Error::Invalid("event cursor"));
    }
    Ok(sequence)
}

pub fn validate_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 160
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-[]".contains(&byte))
    {
        return Err(Error::Invalid("projection identifier"));
    }
    Ok(())
}

fn redaction_name(value: &str) -> bool {
    matches!(
        value,
        "prompt" | "detail" | "workspace" | "provider" | "model" | "arguments" | "command"
    )
}

/// Apply capability rules to a status projection.  Redaction is omission,
/// not a sentinel string, so a consumer cannot mistake private data for an
/// empty value.  The returned value is newly allocated and safe to serialize.
pub fn redact_status(
    value: &StatusProjection,
    capabilities: &CapabilitySet,
) -> Result<StatusProjection> {
    capabilities.validate()?;
    capabilities.require(Capability::StatusRead)?;
    let mut output = value.clone();
    let mut redactions = BTreeSet::new();
    if !capabilities.contains(Capability::TaskRead) {
        for task in &mut output.tasks {
            for field in ["prompt", "detail", "title"] {
                redactions.insert(format!("tasks.{field}"));
            }
            task.prompt = None;
            task.detail = None;
            task.title = None;
        }
    }
    if !capabilities.contains(Capability::WorkspaceRead) {
        for task in &mut output.tasks {
            task.workspace = None;
        }
        redactions.insert("tasks.workspace".to_owned());
    }
    if !capabilities.contains(Capability::AccountRead) {
        for task in &mut output.tasks {
            task.provider = None;
            task.model = None;
        }
        redactions.insert("tasks.provider".to_owned());
        redactions.insert("tasks.model".to_owned());
    }
    output.redactions = redactions.into_iter().collect();
    validate_status(&output)?;
    Ok(output)
}

pub fn redact_event(
    value: &EventProjection,
    capabilities: &CapabilitySet,
) -> Result<EventProjection> {
    capabilities.validate()?;
    capabilities.require(Capability::EventsRead)?;
    let mut output = value.clone();
    let mut redacted = false;
    if !capabilities.contains(Capability::CommandInspect) {
        output.data = redact_value(&output.data, &mut redacted);
    }
    if !capabilities.contains(Capability::WorkspaceRead) {
        output.data = redact_named(&output.data, "workspace", &mut redacted);
    }
    output.redacted |= redacted;
    Ok(output)
}

/// Redact a generic JSON payload using names rather than provider-specific
/// schemas.  Secrets are always removed, even for `Capability::all()`.
pub fn redact_value(value: &Value, changed: &mut bool) -> Value {
    match value {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_value(item, changed))
                .collect(),
        ),
        Value::Object(map) => {
            let mut result = Map::new();
            for (key, item) in map {
                if is_secret_key(key) {
                    *changed = true;
                    continue;
                }
                if redaction_name(key) {
                    *changed = true;
                    continue;
                }
                result.insert(key.clone(), redact_value(item, changed));
            }
            Value::Object(result)
        }
        other => other.clone(),
    }
}

fn redact_named(value: &Value, name: &str, changed: &mut bool) -> Value {
    match value {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_named(item, name, changed))
                .collect(),
        ),
        Value::Object(map) => {
            let mut result = Map::new();
            for (key, item) in map {
                if key == name {
                    *changed = true;
                    continue;
                }
                result.insert(key.clone(), redact_named(item, name, changed));
            }
            Value::Object(result)
        }
        other => other.clone(),
    }
}

fn is_secret_key(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("token")
        || lower.contains("secret")
        || lower.contains("password")
        || lower.contains("credential")
        || lower == "authorization"
        || lower == "access_key"
}

pub fn projection_size<T: Serialize>(value: &T) -> Result<usize> {
    let json = serde_json::to_value(value).map_err(|_| Error::Invalid("projection JSON"))?;
    let encoded = protocol::canonical_json(&json)?;
    Ok(encoded.len())
}

pub fn validate_status(value: &StatusProjection) -> Result<()> {
    check_schema(
        &value.schema,
        &value.schema_digest,
        STATUS_PROJECTION_SCHEMA,
    )?;
    if value.tasks.len() > MAX_TASKS {
        return Err(Error::Limit("status tasks"));
    }
    if value.tasks.windows(2).any(|pair| pair[0].id >= pair[1].id) {
        return Err(Error::Invalid("status task order"));
    }
    validate_id(&value.factory.id)?;
    if value.factory.revision > value.revision {
        return Err(Error::Invalid("status revision"));
    }
    if projection_size(value)? > MAX_STATUS_PROJECTION_BYTES {
        return Err(Error::Limit("status projection"));
    }
    Ok(())
}

pub fn validate_event_page(value: &EventPage) -> Result<()> {
    check_schema(&value.schema, &value.schema_digest, EVENT_PROJECTION_SCHEMA)?;
    if value.events.len() > MAX_EVENT_PAGE_SIZE {
        return Err(Error::Limit("event page"));
    }
    if !value.has_more && value.next_cursor.is_some() {
        return Err(Error::Invalid("event page cursor"));
    }
    if value.has_more && value.next_cursor.is_none() {
        return Err(Error::Invalid("event page cursor"));
    }
    if let Some(cursor) = value.next_cursor.as_deref() {
        decode_cursor(cursor)?;
    }
    if value
        .events
        .windows(2)
        .any(|pair| pair[0].sequence >= pair[1].sequence)
    {
        return Err(Error::Invalid("event sequence order"));
    }
    for event in &value.events {
        check_schema(&event.schema, &event.schema_digest, EVENT_PROJECTION_SCHEMA)?;
        validate_id(&event.id)?;
        validate_id(&event.entity_id)?;
    }
    if projection_size(value)? > MAX_EVENT_PAGE_BYTES {
        return Err(Error::Limit("event projection"));
    }
    Ok(())
}

/// A stable digest helper for callers that want to pin the schema in a
/// manifest.  It is intentionally just the protocol descriptor digest, not a
/// digest of mutable status data.
pub fn schema_digest_bytes() -> [u8; 32] {
    let hex = schema_digest();
    let mut out = [0u8; 32];
    for (index, chunk) in hex.as_bytes().chunks_exact(2).enumerate() {
        out[index] = (hex_digit(chunk[0]) << 4) | hex_digit(chunk[1]);
    }
    out
}
fn hex_digit(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn schema_digest_is_stable_and_capabilities_are_sorted() {
        let digest = schema_digest();
        assert_eq!(digest.len(), 64);
        check_schema_digest(&digest).expect("digest");
        let caps = CapabilitySet::from_iter([Capability::EventsRead, Capability::StatusRead])
            .expect("caps");
        assert!(caps.contains(Capability::StatusRead));
        assert_eq!(caps.names(), ["status-read", "events-read"]);
        assert!(
            CapabilitySet::from_iter([Capability::StatusRead, Capability::EventsRead])
                .unwrap()
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn public_redaction_never_leaks_secret_or_private_task_fields() {
        let status = StatusProjection {
            schema: STATUS_PROJECTION_SCHEMA.into(),
            schema_digest: schema_digest(),
            revision: 1,
            factory: FactoryProjection {
                id: "factory_ref".into(),
                state: "ready".into(),
                revision: 1,
                capabilities: CapabilitySet::public().names(),
            },
            tasks: vec![TaskProjection {
                id: "task_ref".into(),
                revision: 1,
                state: "queued".into(),
                title: Some("private title".into()),
                workspace: Some("/private/project".into()),
                prompt: Some("private prompt".into()),
                provider: Some("claude".into()),
                model: Some("model".into()),
                detail: Some("private detail".into()),
            }],
            redactions: vec![],
        };
        let public = redact_status(&status, &CapabilitySet::public()).expect("public");
        assert!(public.tasks[0].prompt.is_none());
        assert!(public.tasks[0].workspace.is_none());
        assert!(!serde_json::to_string(&public).unwrap().contains("private"));
        validate_status(&public).expect("valid");
        let mut changed = false;
        let value = redact_value(
            &json!({"token":"secret","nested":{"password":"x","ok":true}}),
            &mut changed,
        );
        assert!(changed);
        assert_eq!(value, json!({"nested":{"ok":true}}));
    }

    #[test]
    fn bounded_event_page_requires_cursor_when_more() {
        let event = EventProjection {
            schema: EVENT_PROJECTION_SCHEMA.into(),
            schema_digest: schema_digest(),
            sequence: 1,
            id: "evt_ref_1".into(),
            revision: 1,
            entity_id: "factory_ref".into(),
            kind: "task_accepted".into(),
            data: json!({"ok": true}),
            redacted: false,
        };
        let page = EventPage {
            schema: EVENT_PROJECTION_SCHEMA.into(),
            schema_digest: schema_digest(),
            events: vec![event],
            next_cursor: Some("e:1".into()),
            has_more: true,
        };
        validate_event_page(&page).expect("page");
        let invalid = EventPage {
            next_cursor: None,
            ..page
        };
        assert!(validate_event_page(&invalid).is_err());
    }
}
