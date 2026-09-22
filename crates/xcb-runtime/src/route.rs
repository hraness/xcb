//! Closed machine routing contract: one task in, one settled routed turn out.
//! The request chooses eligibility constraints only; provider admission,
//! account custody, and workspace confinement remain with the runtime.
//! Failure output is deliberately sanitized — provider payloads, stderr, and
//! private paths never cross this protocol.
use crate::{
    Error, Result,
    config::Config,
    kernel, new_id, routing,
    runner::{Observer, Outcome},
    store::Store,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::PathBuf, sync::Arc};
use tokio::sync::watch;
use xcb_core::{
    Id, MAX_TEXT_BYTES, Provider, display_text,
    policy::{EffectState, Terminal, TurnFacts},
    session::State,
};

pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
pub const MIN_TIMEOUT_MS: u64 = 1_000;
pub const MAX_TIMEOUT_MS: u64 = 3_600_000;
const MAX_REASON_BYTES: usize = 512;
const MAX_LABEL_BYTES: usize = 160;
const MAX_ACCOUNT_BYTES: usize = 160;
const MAX_MODEL_BYTES: usize = 512;
const MAX_WORKSPACE_BYTES: usize = 4096;

/// One closed request document. `provider`, `account`, and `model` are
/// optional pins; absent pins leave selection to the eligible-route ranking.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteTaskRequest {
    pub version: u32,
    /// Absolute workspace directory for the routed turn.
    pub workspace: String,
    pub task: String,
    #[serde(default)]
    pub provider: Option<Provider>,
    /// Account id or exact observed name; resolved against local accounts.
    #[serde(default)]
    pub account: Option<String>,
    /// Full observed model key, e.g. `claude/sonnet/low`.
    #[serde(default)]
    pub model: Option<String>,
    /// Optional caller deadline; on expiry the turn is cancelled and the
    /// response is emitted only after custody settles.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Select and report a route without creating a session or run.
    #[serde(default)]
    pub dry_run: bool,
}

impl RouteTaskRequest {
    pub fn parse(bytes: &[u8]) -> std::result::Result<Self, RouteCode> {
        if bytes.is_empty() || bytes.len() > MAX_REQUEST_BYTES {
            return Err(RouteCode::InvalidRequest);
        }
        let request: Self = serde_json::from_slice(bytes).map_err(|_| RouteCode::InvalidRequest)?;
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> std::result::Result<(), RouteCode> {
        let short = |value: &Option<String>, max: usize| {
            value.as_deref().is_none_or(|value| {
                !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
            })
        };
        if self.version != 1
            || self.task.is_empty()
            || self.task.len() > MAX_TEXT_BYTES
            || self.task.contains('\0')
            || self.workspace.is_empty()
            || self.workspace.len() > MAX_WORKSPACE_BYTES
            || self.workspace.contains('\0')
            || !short(&self.account, MAX_ACCOUNT_BYTES)
            || !short(&self.model, MAX_MODEL_BYTES)
            || self
                .timeout_ms
                .is_some_and(|ms| !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&ms))
        {
            return Err(RouteCode::InvalidRequest);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteCode {
    InvalidRequest,
    Unavailable,
    Busy,
    Deadline,
    Cancelled,
    ProviderError,
    CustodyUnproven,
    NeedsInput,
}

/// The selected eligible route. `reason` is a heuristic description, not a
/// provider or billing guarantee.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteTaken {
    pub provider: Provider,
    pub account: Id,
    /// Full observed model key.
    pub model: String,
    pub label: String,
    pub reason: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteResponse {
    pub version: u32,
    /// `completed` for a settled successful turn, `selected` for a dry run.
    pub status: &'static str,
    pub request_id: String,
    pub route: RouteTaken,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<State>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<TurnFacts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_truncated: Option<bool>,
}

impl RouteResponse {
    fn selected(request_id: &Id, route: RouteTaken) -> Self {
        Self {
            version: 1,
            status: "selected",
            request_id: request_id.to_string(),
            route,
            session: None,
            state: None,
            outcome: None,
            text: None,
            text_truncated: None,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteFailure {
    pub version: u32,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub code: RouteCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<RouteTaken>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<TurnFacts>,
    /// The turn's final text is included only for `needs_input`, where it
    /// carries the question the caller must answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_truncated: Option<bool>,
    /// Present only when this request provably launched no provider process.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub joined: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effects: Option<&'static str>,
}

impl RouteFailure {
    fn new(code: RouteCode) -> Self {
        Self {
            version: 1,
            status: "failed",
            request_id: None,
            code,
            route: None,
            session: None,
            outcome: None,
            text: None,
            text_truncated: None,
            joined: None,
            effects: None,
        }
    }
    /// This request provably created no provider process or helper. Local to
    /// the request; it says nothing about other work using the account.
    pub fn unstarted(code: RouteCode) -> Self {
        if code == RouteCode::CustodyUnproven {
            Self::new(code)
        } else {
            let mut failure = Self::new(code);
            failure.joined = Some(true);
            failure.effects = Some("none");
            failure
        }
    }
    /// A session/run record exists; custody claims come only from recorded
    /// outcome facts, never from this failure object.
    fn started(code: RouteCode) -> Self {
        Self::new(code)
    }
    fn bound(mut self, id: &Id) -> Self {
        self.request_id = Some(id.to_string());
        self
    }
    fn routed(mut self, route: RouteTaken) -> Self {
        self.route = Some(route);
        self
    }
    fn settled_turn(mut self, session: Id, outcome: Outcome) -> Self {
        self.session = Some(session);
        self.outcome = Some(outcome.facts);
        if self.code == RouteCode::NeedsInput {
            self.text_truncated = Some(outcome.text.len() > MAX_TEXT_BYTES);
            self.text = Some(display_text(&outcome.text, MAX_TEXT_BYTES));
        }
        self
    }
}

/// Select one eligible route, then run exactly one bounded turn on it. No
/// continuation, failover, hooks expansion, or retry happens inside this call;
/// a caller that wants another route invokes the contract again.
pub async fn dispatch(
    store: Arc<Store>,
    request: RouteTaskRequest,
    cancel: watch::Receiver<bool>,
    observer: Observer,
) -> std::result::Result<RouteResponse, Box<RouteFailure>> {
    request
        .validate()
        .map_err(|code| Box::new(RouteFailure::unstarted(code)))?;
    let id = new_id("route");
    let fail = |code| RouteFailure::unstarted(code).bound(&id);
    if *cancel.borrow() {
        return Err(Box::new(fail(RouteCode::Cancelled)));
    }
    let workspace = PathBuf::from(&request.workspace)
        .canonicalize()
        .ok()
        .filter(|path| path.is_dir())
        .ok_or_else(|| Box::new(fail(RouteCode::InvalidRequest)))?;
    let account = match request.account.as_deref() {
        Some(value) => Some(
            store
                .resolve_account(value)
                .map_err(|_| Box::new(fail(RouteCode::Unavailable)))?,
        ),
        None => None,
    };
    if let (Some(provider), Some(account)) = (request.provider, &account)
        && account.provider != provider
    {
        return Err(Box::new(fail(RouteCode::InvalidRequest)));
    }
    let required_provider = request
        .provider
        .or_else(|| account.as_ref().map(|account| account.provider));
    let config = Config::load(store.root())
        .map_err(|_| Box::new(fail(RouteCode::Unavailable)))?
        .0;
    let excluded_routes = BTreeSet::new();
    let excluded_accounts = BTreeSet::new();
    let decision = routing::smart_route(
        &store,
        &config,
        routing::RouteRequest {
            task: &request.task,
            required_provider,
            preferred_provider: None,
            required_model: request.model.as_deref(),
            excluded_routes: &excluded_routes,
            excluded_accounts: &excluded_accounts,
            account: account.as_ref().map(|account| &account.id),
        },
    )
    .await
    .map_err(|error| {
        Box::new(fail(match error {
            Error::Conflict(_) => RouteCode::Busy,
            _ => RouteCode::Unavailable,
        }))
    })?;
    let route = RouteTaken {
        provider: decision.model.provider,
        account: decision.account,
        model: decision.model.key(),
        label: display_text(&decision.model.label, MAX_LABEL_BYTES),
        reason: display_text(&decision.reason, MAX_REASON_BYTES),
    };
    if request.dry_run {
        return Ok(RouteResponse::selected(&id, route));
    }
    if *cancel.borrow() {
        return Err(Box::new(fail(RouteCode::Cancelled).routed(route)));
    }
    let session = kernel::new_session(
        &store,
        &workspace,
        &config,
        Some(&route.account),
        Some(&route.model),
    )
    .map_err(|error| {
        Box::new(
            fail(match error {
                Error::Conflict(_) => RouteCode::Busy,
                _ => RouteCode::Unavailable,
            })
            .routed(route.clone()),
        )
    })?;
    let outcome = kernel::execute_once(
        store,
        session.id.clone(),
        request.task,
        vec![],
        cancel.clone(),
        observer,
    )
    .await;
    settle(&id, session.id, route, outcome, *cancel.borrow())
}

/// Classify a finished dispatch. Unsettled custody dominates every other
/// outcome; a provider asking for input is reported, not answered.
fn settle(
    id: &Id,
    session: Id,
    route: RouteTaken,
    outcome: Result<Outcome>,
    cancelled: bool,
) -> std::result::Result<RouteResponse, Box<RouteFailure>> {
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            let code = match error {
                _ if cancelled => RouteCode::Cancelled,
                Error::Conflict(_) => RouteCode::Busy,
                Error::CleanupUnproven => RouteCode::CustodyUnproven,
                Error::LaunchNotStarted(_)
                | Error::Protocol(_)
                | Error::CodexRpc { .. }
                | Error::DevinRpc { .. }
                | Error::DevinModelChoices { .. } => RouteCode::ProviderError,
                _ => RouteCode::Unavailable,
            };
            return Err(Box::new(
                RouteFailure::started(code)
                    .bound(id)
                    .routed(route)
                    .settled_session(session),
            ));
        }
    };
    let facts = outcome.facts.clone();
    let failure = if !facts.joined || facts.effects == EffectState::Uncertain {
        Some(RouteCode::CustodyUnproven)
    } else if cancelled || facts.terminal == Terminal::Cancelled {
        Some(RouteCode::Cancelled)
    } else if facts.pending_attention
        || matches!(
            outcome.state,
            State::NeedsAnswer | State::NeedsAction | State::NeedsApproval
        )
    {
        Some(RouteCode::NeedsInput)
    } else if facts.terminal == Terminal::Completed
        && facts.failure.is_none()
        && outcome.state == State::Idle
    {
        None
    } else {
        Some(RouteCode::ProviderError)
    };
    match failure {
        Some(code) => Err(Box::new(
            RouteFailure::started(code)
                .bound(id)
                .routed(route)
                .settled_turn(session, outcome),
        )),
        None => {
            let truncated = outcome.text.len() > MAX_TEXT_BYTES;
            Ok(RouteResponse {
                version: 1,
                status: "completed",
                request_id: id.to_string(),
                route,
                session: Some(session),
                state: Some(outcome.state),
                outcome: Some(facts),
                text: Some(display_text(&outcome.text, MAX_TEXT_BYTES)),
                text_truncated: Some(truncated).filter(|truncated| *truncated),
            })
        }
    }
}

impl RouteFailure {
    /// Attach a session id without outcome facts (execution never returned).
    fn settled_session(mut self, session: Id) -> Self {
        self.session = Some(session);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> RouteTaskRequest {
        RouteTaskRequest {
            version: 1,
            workspace: "/tmp".into(),
            task: "fix the failing test".into(),
            provider: None,
            account: None,
            model: None,
            timeout_ms: None,
            dry_run: false,
        }
    }

    #[test]
    fn closed_schema_rejects_unknown_and_bad_fields() {
        assert!(
            RouteTaskRequest::parse(br#"{"version":1,"workspace":"/tmp","task":"t","extra":1}"#)
                .is_err()
        );
        assert!(RouteTaskRequest::parse(b"{}").is_err());
        assert!(RouteTaskRequest::parse(b"").is_err());
        assert!(RouteTaskRequest::parse(&vec![b' '; MAX_REQUEST_BYTES + 1]).is_err());
        for bad in [
            br#"{"version":2,"workspace":"/tmp","task":"t"}"#.as_slice(),
            br#"{"version":1,"workspace":"","task":"t"}"#.as_slice(),
            br#"{"version":1,"workspace":"/tmp","task":""}"#.as_slice(),
            br#"{"version":1,"workspace":"/tmp","task":"t","timeoutMs":0}"#.as_slice(),
            br#"{"version":1,"workspace":"/tmp","task":"t","provider":"auto"}"#.as_slice(),
            br#"{"version":1,"workspace":"/tmp","task":"t","dryRun":"yes"}"#.as_slice(),
        ] {
            assert!(RouteTaskRequest::parse(bad).is_err(), "{bad:?}");
        }
        let ok = RouteTaskRequest::parse(
            br#"{"version":1,"workspace":"/tmp","task":"t","provider":"claude","timeoutMs":60000,"dryRun":true}"#,
        )
        .unwrap();
        assert!(ok.dry_run);
        assert_eq!(ok.provider, Some(Provider::Claude));
        assert_eq!(ok.timeout_ms, Some(60_000));
    }

    #[test]
    fn validate_bounds() {
        assert!(request().validate().is_ok());
        let mut value = request();
        value.timeout_ms = Some(MIN_TIMEOUT_MS - 1);
        assert!(value.validate().is_err());
        let mut value = request();
        value.timeout_ms = Some(MAX_TIMEOUT_MS + 1);
        assert!(value.validate().is_err());
        let mut value = request();
        value.model = Some("x".repeat(MAX_MODEL_BYTES + 1));
        assert!(value.validate().is_err());
        let mut value = request();
        value.task = "t".repeat(MAX_TEXT_BYTES + 1);
        assert!(value.validate().is_err());
    }

    #[tokio::test]
    async fn an_empty_store_reports_no_eligible_route() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("work");
        std::fs::create_dir(&workspace).unwrap();
        let store =
            Arc::new(Store::open(&directory.path().canonicalize().unwrap().join("state")).unwrap());
        let (_send, cancel) = watch::channel(false);
        let observer: Observer = Arc::new(|_| ());
        let mut request = request();
        request.workspace = workspace.to_string_lossy().into_owned();
        let result = dispatch(store, request, cancel, observer).await;
        let failure = result.unwrap_err();
        assert_eq!(failure.code, RouteCode::Unavailable);
        assert_eq!(failure.joined, Some(true));
        assert_eq!(failure.effects, Some("none"));
    }

    #[tokio::test]
    async fn a_missing_workspace_is_an_invalid_request() {
        let directory = tempfile::tempdir().unwrap();
        let store =
            Arc::new(Store::open(&directory.path().canonicalize().unwrap().join("state")).unwrap());
        let (_send, cancel) = watch::channel(false);
        let observer: Observer = Arc::new(|_| ());
        let mut request = request();
        request.workspace = directory
            .path()
            .join("missing")
            .to_string_lossy()
            .into_owned();
        let failure = dispatch(store, request, cancel, observer)
            .await
            .unwrap_err();
        assert_eq!(failure.code, RouteCode::InvalidRequest);
    }
}
