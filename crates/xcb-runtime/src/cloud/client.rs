//! The Convex transport lane: a `ConvexClient` wrapper that speaks
//! `serde_json::Value` at the boundary, carries the device session token,
//! and surfaces the relay's closed error vocabulary from `ConvexError.data`.
//!
//! The JSON ↔ `convex::Value` bridge lives in `wire` — the crate's own
//! conversions are the Convex wire encoding (`$integer` markers), not plain
//! JSON.

use std::collections::BTreeMap;

use convex::{ConvexClient, FunctionResult};
use serde_json::Value;

use crate::{Error, Result};

use super::wire::{to_convex, to_json};

fn protocol(what: &'static str) -> Error {
    Error::Protocol(what)
}

/// A leaked, bounded protocol detail for errors that arrive from the relay
/// or transport with dynamic text. Leaks only on the error path.
fn dynamic(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

fn object_args(entries: Vec<(&str, Value)>) -> Result<BTreeMap<String, convex::Value>> {
    entries
        .into_iter()
        .map(|(key, value)| Ok((key.to_string(), to_convex(&value)?)))
        .collect()
}

/// Unwrap a `FunctionResult`: `Value` becomes JSON; `ConvexError` surfaces
/// the relay's typed `data.code` when present so callers can match the
/// closed vocabulary; `ErrorMessage` is opaque server text.
fn unwrap(result: FunctionResult) -> Result<Value> {
    match result {
        FunctionResult::Value(value) => to_json(&value),
        FunctionResult::ConvexError(error) => {
            let code = to_json(&error.data)
                .ok()
                .and_then(|data| data.get("code").and_then(Value::as_str).map(str::to_string));
            Err(match code {
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
}

impl RelayClient {
    /// Connect unauthenticated (the OTP flow starts here).
    pub async fn connect(deployment_url: &str) -> Result<Self> {
        let client = ConvexClient::new(deployment_url)
            .await
            .map_err(|error| protocol(dynamic(format!("convex connect: {error}"))))?;
        Ok(Self { client })
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
        let result = self
            .client
            .mutation(path, object_args(args)?)
            .await
            .map_err(|error| protocol(dynamic(format!("relay mutation {path}: {error}"))))?;
        unwrap(result)
    }

    /// Call a public query.
    pub async fn query(&mut self, path: &str, args: Vec<(&str, Value)>) -> Result<Value> {
        let result = self
            .client
            .query(path, object_args(args)?)
            .await
            .map_err(|error| protocol(dynamic(format!("relay query {path}: {error}"))))?;
        unwrap(result)
    }

    /// Call a public action — Convex Auth's `signIn` is an action.
    pub async fn action(&mut self, path: &str, args: Vec<(&str, Value)>) -> Result<Value> {
        let result = self
            .client
            .action(path, object_args(args)?)
            .await
            .map_err(|error| protocol(dynamic(format!("relay action {path}: {error}"))))?;
        unwrap(result)
    }
}
