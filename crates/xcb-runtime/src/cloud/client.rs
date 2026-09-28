//! The Convex transport lane: a `ConvexClient` wrapper that speaks
//! `serde_json::Value` at the boundary, carries the device session token,
//! and surfaces the relay's closed error vocabulary from `ConvexError.data`.
//!
//! The JSON ↔ `convex::Value` bridge lives in `wire` — the crate's own
//! conversions are the Convex wire encoding (`$integer` markers), not plain
//! JSON.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::Duration;

use convex::{ConvexClient, FunctionResult};
use serde_json::Value;

use crate::{Error, Result};

use super::wire::{to_convex, to_json};

/// Every relay call is bounded: the crate's worker awaits a oneshot that a
/// poisoned websocket can leave hanging forever, so a stalled transport
/// must surface here rather than wedge a pump tick or a controller CLI.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Distinct dynamic error texts kept for the process lifetime; one text is
/// stored once however often it recurs.
const MAX_DYNAMIC_TEXTS: usize = 256;
const MAX_DYNAMIC_CHARS: usize = 300;

fn protocol(what: &'static str) -> Error {
    Error::Protocol(what)
}

fn timed_out() -> Error {
    Error::Unavailable("relay request timed out")
}

/// A bounded protocol detail for errors that arrive from the relay or
/// transport with dynamic text. Each distinct text is leaked once and
/// reused, and the set is capped, so a failure that recurs for the life of
/// the supervisor cannot grow its memory.
fn dynamic(text: String) -> &'static str {
    static TEXTS: Mutex<BTreeSet<&'static str>> = Mutex::new(BTreeSet::new());
    let text: String = text.chars().take(MAX_DYNAMIC_CHARS).collect();
    let Ok(mut texts) = TEXTS.lock() else {
        return "relay error (detail unavailable)";
    };
    if let Some(known) = texts.get(text.as_str()) {
        return known;
    }
    if texts.len() >= MAX_DYNAMIC_TEXTS {
        return "relay error (detail omitted)";
    }
    let leaked: &'static str = Box::leak(text.into_boxed_str());
    texts.insert(leaked);
    leaked
}

/// The relay's `field` for an `invalid-argument` error, when it has the
/// wire shape (`@hraness/relay` `wire/errors.ts`): it names which argument
/// the relay refused, for example `device` for a lapsed presence row.
fn argument_field(data: &Value) -> Option<&str> {
    let field = data.get("field")?.as_str()?;
    let mut bytes = field.bytes();
    (field.len() <= 64
        && bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'.'))
    .then_some(field)
}

fn object_args(entries: Vec<(&str, Value)>) -> Result<BTreeMap<String, convex::Value>> {
    entries
        .into_iter()
        .map(|(key, value)| Ok((key.to_string(), to_convex(&value)?)))
        .collect()
}

/// Unwrap a `FunctionResult`: `Value` becomes JSON; `ConvexError` surfaces
/// the relay's typed `data.code` when present so callers can match the
/// closed vocabulary (`relay <code>`, plus `: <field>` for an
/// `invalid-argument`, so a diagnosis names the refused argument);
/// `ErrorMessage` is opaque server text.
fn unwrap(result: FunctionResult) -> Result<Value> {
    match result {
        FunctionResult::Value(value) => to_json(&value),
        FunctionResult::ConvexError(error) => {
            let data = to_json(&error.data).ok();
            let code = data
                .as_ref()
                .and_then(|data| data.get("code").and_then(Value::as_str));
            Err(match code {
                Some("invalid-argument") => {
                    Error::Protocol(match data.as_ref().and_then(argument_field) {
                        Some(field) => dynamic(format!("relay invalid-argument: {field}")),
                        None => "relay invalid-argument",
                    })
                }
                Some(code) => Error::Protocol(dynamic(format!("relay {code}"))),
                None => Error::Protocol(dynamic(format!("relay error: {}", error.message))),
            })
        }
        FunctionResult::ErrorMessage(message) => {
            Err(Error::Protocol(dynamic(format!("relay error: {message}"))))
        }
    }
}

/// A relay-bound Convex client for one device session. Function calls are
/// untyped at the transport layer — `wire/` validators parse every row
/// before it is trusted, matching `client-ts` in the relay package.
pub struct RelayClient {
    client: ConvexClient,
    pub(super) deployment_url: String,
    pub(super) session_binding: Option<super::custody::SessionBinding>,
}

impl RelayClient {
    /// Connect unauthenticated (the OTP flow starts here).
    pub async fn connect(deployment_url: &str) -> Result<Self> {
        let client = tokio::time::timeout(REQUEST_TIMEOUT, ConvexClient::new(deployment_url))
            .await
            .map_err(|_| timed_out())?
            .map_err(|error| protocol(dynamic(format!("convex connect: {error}"))))?;
        Ok(Self {
            client,
            deployment_url: deployment_url.to_owned(),
            session_binding: None,
        })
    }

    /// Bind the credentials a caller loaded before connecting. Re-reading
    /// custody must never silently move an already-open device to new keys.
    pub(super) fn bind_session_keys(
        &mut self,
        state_root: &std::path::Path,
        session: &super::custody::CloudSession,
        device: &super::crypto::DeviceIdentity,
        account: &super::crypto::AccountKey,
        key_version: u64,
    ) -> Result<()> {
        let binding =
            super::custody::SessionBinding::capture(state_root, &self.deployment_url, session)?;
        binding.expect_keys(state_root, device, account, key_version)?;
        binding.check(state_root, session)?;
        self.session_binding = Some(binding);
        Ok(())
    }

    /// Attach the session token so authenticated calls carry it.
    pub async fn authenticate(&mut self, token: &str) {
        self.client.set_auth(Some(token.to_string())).await;
    }

    /// Drop the session token (sign-out, re-auth).
    pub async fn clear_auth(&mut self) {
        self.client.set_auth(None).await;
    }

    /// Call a public mutation by `module:name` with a JSON object of args.
    pub async fn mutation(&mut self, path: &str, args: Vec<(&str, Value)>) -> Result<Value> {
        let result = tokio::time::timeout(
            REQUEST_TIMEOUT,
            self.client.mutation(path, object_args(args)?),
        )
        .await
        .map_err(|_| timed_out())?
        .map_err(|error| protocol(dynamic(format!("relay mutation {path}: {error}"))))?;
        unwrap(result)
    }

    /// Call a public query.
    pub async fn query(&mut self, path: &str, args: Vec<(&str, Value)>) -> Result<Value> {
        let result =
            tokio::time::timeout(REQUEST_TIMEOUT, self.client.query(path, object_args(args)?))
                .await
                .map_err(|_| timed_out())?
                .map_err(|error| protocol(dynamic(format!("relay query {path}: {error}"))))?;
        unwrap(result)
    }

    /// Call a public action — Convex Auth's `signIn` is an action.
    pub async fn action(&mut self, path: &str, args: Vec<(&str, Value)>) -> Result<Value> {
        let result = tokio::time::timeout(
            REQUEST_TIMEOUT,
            self.client.action(path, object_args(args)?),
        )
        .await
        .map_err(|_| timed_out())?
        .map_err(|error| protocol(dynamic(format!("relay action {path}: {error}"))))?;
        unwrap(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn relay_error(data: Value) -> Error {
        unwrap(FunctionResult::ConvexError(convex::ConvexError {
            message: "refused".into(),
            data: to_convex(&data).unwrap(),
        }))
        .unwrap_err()
    }

    #[test]
    fn relay_codes_keep_their_match_strings_and_name_a_refused_argument() {
        assert!(matches!(
            relay_error(json!({"code":"unauthenticated"})),
            Error::Protocol("relay unauthenticated")
        ));
        assert!(matches!(
            relay_error(json!({"code":"conflict","field":"expectedRevision"})),
            Error::Protocol("relay conflict")
        ));
        assert!(matches!(
            relay_error(json!({"code":"invalid-argument","field":"device"})),
            Error::Protocol("relay invalid-argument: device")
        ));
        // A field outside the wire grammar is never echoed.
        assert!(matches!(
            relay_error(json!({"code":"invalid-argument","field":"Bad Field!"})),
            Error::Protocol("relay invalid-argument")
        ));
    }

    #[test]
    fn a_recurring_relay_error_reuses_one_stored_text() {
        let first = dynamic("relay error: synthetic recurring failure".into());
        let again = dynamic("relay error: synthetic recurring failure".into());
        assert!(std::ptr::eq(first, again));
        assert!(dynamic("x".repeat(10_000)).chars().count() <= MAX_DYNAMIC_CHARS);
    }
}
