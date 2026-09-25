//! The xcb wire contract over `hraness/relay`: identifier shapes, authority
//! tuples, command lifecycle, and envelope validation — the Rust mirror of
//! `wire/` validators. Every value arriving from the relay parses through
//! here before it is trusted.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, Result};

fn invalid(what: &'static str) -> Error {
    Error::from(xcb_core::Error::Invalid(what))
}

/// Convex `v.number()` columns arrive as `Float64` — `1` comes back `1.0`,
/// which serde refuses as `u64`. Every integer field that crosses in from
/// the relay deserializes through this: integral floats only.
mod de {
    use serde::{Deserialize, Deserializer};

    pub fn u64<'de, D>(deserializer: D) -> Result<u64, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        if let Some(int) = value.as_u64() {
            return Ok(int);
        }
        if let Some(float) = value.as_f64()
            && float.is_finite()
            && float.fract() == 0.0
            && float >= 0.0
            && float <= u64::MAX as f64
        {
            return Ok(float as u64);
        }
        Err(serde::de::Error::custom("expected an integer"))
    }
}

/// Read an integer from a wire `Value`: Convex `v.number()` arrives as
/// `Float64`, so both representations are accepted but only integral
/// floats. Returns `None` for anything else.
pub fn json_u64(value: &Value) -> Option<u64> {
    if let Some(int) = value.as_u64() {
        return Some(int);
    }
    match value.as_f64() {
        Some(float)
            if float.is_finite()
                && float.fract() == 0.0
                && float >= 0.0
                && float <= u64::MAX as f64 =>
        {
            Some(float as u64)
        }
        _ => None,
    }
}

pub const NAMESPACE: &str = "xcb.relay.v1";
/// The bind-challenge contract a device signs to finish enrollment.
pub const DEVICE_BIND_CONTRACT: &str = "xcb.relay.v1:device-bind";

/// The closed command union for `xcb.relay.v1`, frozen in
/// `docs/plans/remote-access.md`.
pub const COMMAND_KINDS: &[&str] = &[
    "task_dispatch",
    "task_steer",
    "task_cancel",
    "attention_answer",
    "daemon_send",
    "projection_refresh",
];

/// Lowercase hexadecimal character test (`0-9a-f`) — `is_ascii_hexdigit`
/// alone admits `A-F`, and `is_ascii_lowercase` alone rejects digits.
fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

pub fn is_device_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(is_lower_hex)
}

pub fn is_digest(value: &str) -> bool {
    let rest = value.strip_prefix("sha256:");
    matches!(rest, Some(hex) if hex.len() == 64 && hex.bytes().all(is_lower_hex))
}

pub fn is_public_id(value: &str) -> bool {
    (16..=64).contains(&value.len()) && value.bytes().all(is_lower_hex)
}

pub fn is_opaque_identifier(value: &str) -> bool {
    (22..=128).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// `^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$`
/// — the version/variant bits are part of the wire shape.
pub fn is_uuid_v7(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        match index {
            8 | 13 | 18 | 23 => {
                if *byte != b'-' {
                    return false;
                }
            }
            14 => {
                if *byte != b'7' {
                    return false;
                }
            }
            19 => {
                if !matches!(byte, b'8' | b'9' | b'a' | b'b') {
                    return false;
                }
            }
            _ => {
                if !is_lower_hex(*byte) {
                    return false;
                }
            }
        }
    }
    true
}

pub fn is_command_kind(value: &str) -> bool {
    COMMAND_KINDS.contains(&value)
}

/// The two device classes the xcb deployment admits — daemon devices
/// execute fenced commands, controllers are read/dispatch-only peers.
pub const EXECUTOR_CLASS: &str = "daemon";
pub const CONTROLLER_CLASS: &str = "controller";

/// `user + device + auth epoch + boot generation` — the tuple the relay
/// fences device commands by. `bootId` is an opaque per-boot string.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorityTuple {
    #[serde(deserialize_with = "de::u64")]
    pub boot_generation: u64,
    pub boot_id: String,
    #[serde(deserialize_with = "de::u64")]
    pub fence: u64,
}

impl AuthorityTuple {
    /// Boot authority for a fresh daemon boot: generation persists across
    /// reboots (custody bumps it), fence starts at zero.
    pub fn boot(generation: u64) -> Self {
        let boot_id = uuid::Uuid::now_v7().simple().to_string();
        Self {
            boot_generation: generation,
            boot_id,
            fence: 0,
        }
    }

    /// Mirrors `compareDeviceAuthority` in `wire/authority.ts`: generation
    /// first, then fence — distinct boot ids at one generation are
    /// incomparable in wall-clock terms, so an equal fence under a
    /// different boot id is treated as later, letting a restarted daemon
    /// make progress rather than deadlocking on its own predecessor.
    pub fn strictly_after(&self, other: &Self) -> bool {
        if self.boot_generation != other.boot_generation {
            return self.boot_generation > other.boot_generation;
        }
        if self.boot_id != other.boot_id {
            return self.fence >= other.fence;
        }
        self.fence > other.fence
    }
}

/// `pending → prepared → effect_started → applied | failed | ambiguous |
/// cancelled | expired`. Terminal states never reopen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandState {
    Pending,
    Prepared,
    EffectStarted,
    Applied,
    Failed,
    Ambiguous,
    Cancelled,
    Expired,
}

impl CommandState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Applied | Self::Failed | Self::Ambiguous | Self::Cancelled | Self::Expired
        )
    }
}

// Envelope shapes -------------------------------------------------------------------

/// A signed `relay.envelope.v1` value as it crosses the wire.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedEnvelope {
    pub contract: String,
    pub sender: String,
    pub recipient: String,
    pub scope: String,
    #[serde(rename = "keyVersion", deserialize_with = "de::u64")]
    pub key_version: u64,
    pub iv: String,
    pub ciphertext: String,
    pub signature: String,
}

/// A signed `relay.keywrap.v1` value.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyWrapEnvelope {
    pub contract: String,
    pub sender: String,
    pub recipient: String,
    #[serde(rename = "keyVersion", deserialize_with = "de::u64")]
    pub key_version: u64,
    pub iv: String,
    pub wrapped: String,
    pub signature: String,
}

/// Validate a decoded signed-envelope value: exact fields are enforced by
/// serde + `deny_unknown_fields` at the call site; this checks the field
/// grammar.
pub fn check_signed_envelope(envelope: &SignedEnvelope, max_ciphertext_chars: usize) -> Result<()> {
    if envelope.contract != super::crypto::RELAY_ENVELOPE_CONTRACT {
        return Err(invalid("envelope contract"));
    }
    if !is_device_id(&envelope.sender) {
        return Err(invalid("envelope sender"));
    }
    if envelope.recipient != "account" && !is_device_id(&envelope.recipient) {
        return Err(invalid("envelope recipient"));
    }
    if envelope.scope.is_empty() || envelope.scope.len() > 128 {
        return Err(invalid("envelope scope"));
    }
    if envelope.key_version < 1 {
        return Err(invalid("envelope key version"));
    }
    let iv = super::crypto::decode_base64url(&envelope.iv, 32)?;
    if iv.len() != super::crypto::IV_BYTES {
        return Err(invalid("envelope iv"));
    }
    if envelope.ciphertext.len() > max_ciphertext_chars {
        return Err(invalid("envelope ciphertext bound"));
    }
    let signature = super::crypto::decode_base64url(&envelope.signature, 128)?;
    if signature.len() != super::crypto::SIGNATURE_BYTES {
        return Err(invalid("envelope signature"));
    }
    Ok(())
}

/// The wire view of a stored command row — the exact `CommandView` shape
/// `relayCommands:get` / `listForTarget` / `listForRequester` return.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandRow {
    pub public_id: String,
    pub kind: String,
    pub state: CommandState,
    pub target_device_id: String,
    pub requesting_device_id: String,
    pub bound_authority: Option<AuthorityTuple>,
    pub deadline: f64,
    pub created_at: f64,
    pub updated_at: f64,
    pub payload: Option<SignedEnvelope>,
    pub result: Option<SignedEnvelope>,
    pub result_code: Option<String>,
}

/// The wire view of a device row — the exact `relayDevices:list` shape.
/// Peer public keys surface here so devices can verify signed envelopes and
/// open key wraps addressed to them.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceRow {
    pub agreement_public_key: String,
    pub device_class: String,
    pub device_id: String,
    #[serde(deserialize_with = "de::u64")]
    pub key_version: u64,
    pub label: String,
    pub online: bool,
    pub signing_public_key: String,
    pub status: String,
}

/// The wire view of a stored projection — the exact `ProjectionView` shape
/// `relayProjections:list` / `get` return.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectionRow {
    pub device_id: String,
    pub public_id: String,
    pub scope: String,
    #[serde(deserialize_with = "de::u64")]
    pub revision: u64,
    pub envelope: SignedEnvelope,
    pub updated_at: f64,
}

// Convex value bridge ---------------------------------------------------------------
//
// The `convex` crate's own `TryFrom<JsonValue>`/`From<Value>` impls are the
// Convex *wire* encoding: every number becomes `Float64` inbound, and
// `Int64`/`Bytes` outbound become `{"$integer": ...}` / `{"$bytes": ...}`
// marker objects. The relay schema declares every numeric field
// `v.number()`, which validates `Float64` only, so the bridge lives here
// and sends every number as `Float64` — the contract's values (ms
// timestamps, versions, fences) are all far below 2^53.

/// Plain JSON → `convex::Value`. Every finite number becomes `Float64`;
/// non-finite numbers never arrive because serde_json cannot represent
/// them.
pub fn to_convex(value: &Value) -> Result<convex::Value> {
    Ok(match value {
        Value::Null => convex::Value::Null,
        Value::Bool(flag) => convex::Value::Boolean(*flag),
        Value::Number(number) => {
            let float = number.as_f64().ok_or(invalid("non-finite argument"))?;
            if !float.is_finite() || float.abs() > 9_007_199_254_740_992.0 {
                return Err(invalid("argument exceeds float64 integer range"));
            }
            convex::Value::Float64(float)
        }
        Value::String(text) => convex::Value::String(text.clone()),
        Value::Array(items) => {
            convex::Value::Array(items.iter().map(to_convex).collect::<Result<Vec<_>>>()?)
        }
        Value::Object(map) => convex::Value::Object(
            map.iter()
                .map(|(key, value)| Ok((key.clone(), to_convex(value)?)))
                .collect::<Result<std::collections::BTreeMap<String, convex::Value>>>()?,
        ),
    })
}

/// `convex::Value` → plain JSON. `Bytes` never cross this contract, so they
/// are rejected rather than guessed at.
pub fn to_json(value: &convex::Value) -> Result<Value> {
    Ok(match value {
        convex::Value::Null => Value::Null,
        convex::Value::Boolean(flag) => Value::Bool(*flag),
        convex::Value::Int64(int) => Value::Number((*int).into()),
        // The contract's numbers are all integers but the wire carries
        // `Float64`. Re-canonicalising a signed envelope must reproduce the
        // exact bytes the sender signed, so integral floats collapse back
        // to integers — `1.0` from the wire is `1` in canonical JSON.
        convex::Value::Float64(float) => {
            let int = *float as i64;
            if float.is_finite() && float.fract() == 0.0 && int as f64 == *float {
                Value::Number(int.into())
            } else {
                Value::Number(
                    serde_json::Number::from_f64(*float)
                        .ok_or(invalid("non-finite value from relay"))?,
                )
            }
        }
        convex::Value::String(text) => Value::String(text.clone()),
        convex::Value::Bytes(_) => return Err(invalid("convex bytes value")),
        convex::Value::Array(items) => {
            Value::Array(items.iter().map(to_json).collect::<Result<Vec<_>>>()?)
        }
        convex::Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| Ok((key.clone(), to_json(value)?)))
                .collect::<Result<serde_json::Map<String, Value>>>()?,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_shapes() {
        assert!(is_device_id(&"a".repeat(32)));
        assert!(!is_device_id(&"a".repeat(31)));
        assert!(!is_device_id(&"A".repeat(32)));
        assert!(is_digest(&format!("sha256:{}", "0".repeat(64))));
        assert!(!is_digest(&"0".repeat(64)));
        assert!(is_command_kind("task_dispatch"));
        assert!(!is_command_kind("exec_shell"));
    }

    #[test]
    fn authority_orders_strictly() {
        let a = AuthorityTuple {
            boot_generation: 1,
            boot_id: "a".repeat(32),
            fence: 0,
        };
        let b = AuthorityTuple {
            boot_generation: 2,
            boot_id: "b".repeat(32),
            fence: 0,
        };
        assert!(b.strictly_after(&a));
        assert!(!a.strictly_after(&b));
        assert!(!a.strictly_after(&a));
        // Same generation, different boot: fence alone orders — an equal
        // fence counts as later so a restarted daemon never deadlocks on
        // its own predecessor (mirrors compareDeviceAuthority).
        let c = AuthorityTuple {
            boot_generation: 2,
            boot_id: "c".repeat(32),
            fence: 0,
        };
        assert!(b.strictly_after(&c));
        assert!(c.strictly_after(&b));
        let c2 = AuthorityTuple {
            fence: 1,
            ..c.clone()
        };
        assert!(c2.strictly_after(&b));
        assert!(!b.strictly_after(&c2));
    }
}
