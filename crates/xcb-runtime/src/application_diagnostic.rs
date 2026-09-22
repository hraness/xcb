//! Private, bounded failure metadata. Never serialize provider error strings.
use crate::{
    Error, Result, digest, now_ms, private,
    store::{RunRecord, Store},
};
use serde::{Deserialize, Serialize};
use std::os::unix::fs::MetadataExt;
use xcb_core::{Id, Provider};

const FILE: &str = "application-diagnostic.json";
const MAX_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Prepare,
    Initialize,
    Start,
    Receive,
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    QuotaOrResourceLimit,
    Authentication,
    Transport,
    LocalAccess,
    ProviderRejected,
    Protocol,
    LocalIo,
    LocalState,
    Unavailable,
    UnexpectedEvent,
    IncompleteOutput,
    ModelUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Initialize,
    SessionNew,
    SessionSetConfigOption,
    SessionSetMode,
    SessionPrompt,
    ThreadStart,
    TurnStart,
    Other,
}

/// A finite vocabulary of host checks, never the original error message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolReason {
    InitializationIdentity,
    AcpImplementation,
    EndedBeforeResult,
    ModelCatalogShape,
    ConfigOptions,
    ModelOption,
    DuplicateModel,
    OptionsAbsent,
    DuplicateConfigOption,
    ModelChanged,
    ModeChanged,
    ModelModeMetadataMissing,
    ModeAcknowledgment,
    EarlyEvent,
    PromptResponseIdentity,
    StopReason,
    UnknownSessionUpdate,
    UnsupportedNotification,
    UnsupportedOutputContent,
    UsageAccounting,
    InvalidEnvelope,
    NativeToolExecuted,
    Other,
}

fn protocol_reason(reason: &str) -> ProtocolReason {
    match reason {
        "Devin initialization response identity" => ProtocolReason::InitializationIdentity,
        "Devin ACP implementation" => ProtocolReason::AcpImplementation,
        "Devin ended before result" => ProtocolReason::EndedBeforeResult,
        "Devin config options" => ProtocolReason::ConfigOptions,
        "Devin model option" => ProtocolReason::ModelOption,
        "Devin duplicate model" => ProtocolReason::DuplicateModel,
        "Devin options absent" => ProtocolReason::OptionsAbsent,
        "Devin duplicate config option" => ProtocolReason::DuplicateConfigOption,
        "Devin model changed" => ProtocolReason::ModelChanged,
        "Devin mode changed" | "Devin mode drift" => ProtocolReason::ModeChanged,
        "Devin model/mode metadata missing" => ProtocolReason::ModelModeMetadataMissing,
        "Devin mode acknowledgment" => ProtocolReason::ModeAcknowledgment,
        "Devin early event" | "Devin tool during initialization" => ProtocolReason::EarlyEvent,
        "Devin prompt response identity" => ProtocolReason::PromptResponseIdentity,
        "Devin stop reason" => ProtocolReason::StopReason,
        "Devin unknown session update" => ProtocolReason::UnknownSessionUpdate,
        "Devin unsupported notification" => ProtocolReason::UnsupportedNotification,
        "Devin unsupported output content" => ProtocolReason::UnsupportedOutputContent,
        "Devin token counter" | "Devin usage mismatch" | "Devin usage regressed" => {
            ProtocolReason::UsageAccounting
        }
        "Devin object"
        | "Devin unknown field"
        | "Devin protocol version"
        | "Devin mixed request response"
        | "Devin malformed response" => ProtocolReason::InvalidEnvelope,
        "Devin native tool executed" => ProtocolReason::NativeToolExecuted,
        _ => ProtocolReason::Other,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Diagnostic {
    version: u32,
    request_id: Id,
    run: Id,
    account: Id,
    provider: Provider,
    recorded_at_ms: u64,
    stage: Stage,
    category: Category,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation: Option<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rpc_code: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    protocol_reason: Option<ProtocolReason>,
}

fn rpc_category(category: &str) -> Category {
    match category {
        "provider quota or resource limit reached" => Category::QuotaOrResourceLimit,
        "authentication rejected; reconnect this account" => Category::Authentication,
        "TLS certificate or transport failure" | "provider request or network failure" => {
            Category::Transport
        }
        "local provider access denied" => Category::LocalAccess,
        _ => Category::ProviderRejected,
    }
}

fn operation(method: &str) -> Operation {
    match method {
        "initialize" => Operation::Initialize,
        "session/new" => Operation::SessionNew,
        "session/set_config_option" => Operation::SessionSetConfigOption,
        "session/set_mode" => Operation::SessionSetMode,
        "session/prompt" => Operation::SessionPrompt,
        "thread/start" => Operation::ThreadStart,
        "turn/start" => Operation::TurnStart,
        _ => Operation::Other,
    }
}

fn classify(error: &Error) -> (Category, Option<Operation>, Option<i64>) {
    let category = match error {
        Error::DevinRpc {
            method,
            code,
            category,
        }
        | Error::CodexRpc {
            method,
            code,
            category,
        } => {
            return (rpc_category(category), Some(operation(method)), Some(*code));
        }
        Error::Protocol(_) => Category::Protocol,
        Error::DevinModelChoices { .. } => Category::ModelUnavailable,
        Error::Io(_) | Error::LaunchNotStarted(_) => Category::LocalIo,
        Error::Unavailable(_) => Category::Unavailable,
        _ => Category::LocalState,
    };
    (category, None, None)
}

/// Best effort only: a diagnostic publication failure must never replace the
/// execution result, skip cleanup, or release an uncertain account lease.
pub(crate) fn record_error(store: &Store, run: &RunRecord, id: &Id, stage: Stage, error: &Error) {
    let (category, operation, rpc_code) = classify(error);
    let protocol_reason = match error {
        Error::Protocol(reason) => Some(protocol_reason(reason)),
        Error::DevinModelChoices { .. } => Some(ProtocolReason::ModelCatalogShape),
        _ => None,
    };
    let _ = record(
        store,
        run,
        id,
        stage,
        (category, operation, rpc_code, protocol_reason),
    );
}

pub(crate) fn record_category(
    store: &Store,
    run: &RunRecord,
    id: &Id,
    stage: Stage,
    category: Category,
) {
    let _ = record(store, run, id, stage, (category, None, None, None));
}

fn record(
    store: &Store,
    run: &RunRecord,
    id: &Id,
    stage: Stage,
    detail: (
        Category,
        Option<Operation>,
        Option<i64>,
        Option<ProtocolReason>,
    ),
) -> Result<()> {
    // The existing exclusive account lease serializes this one-record slot.
    // There is no unbounded log and no new authority independent of the run.
    store.verify_owned_run(run)?;
    let account = store.account(&run.account)?;
    let (category, operation, rpc_code, protocol_reason) = detail;
    let path = store.account_root(&run.account)?.join(FILE);
    let diagnostic = Diagnostic {
        version: 1,
        request_id: id.clone(),
        run: run.id.clone(),
        account: run.account.clone(),
        provider: account.provider,
        recorded_at_ms: now_ms(),
        stage,
        category,
        operation,
        rpc_code,
        protocol_reason,
    };
    let bytes = serde_json::to_vec(&diagnostic)?;
    if bytes.len() > MAX_BYTES {
        return Err(Error::PrivateState);
    }
    match private::read(&path, MAX_BYTES) {
        Ok(previous) => private::replace(&path, &bytes, &digest(previous)),
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            private::create(&path, &bytes)
        }
        Err(error) => Err(error),
    }
}

/// Read only an exact request's latest retained diagnostic. The record is not
/// settlement evidence and is replaced by the next diagnosed account failure.
pub fn read(store: &Store, account: &Id, request: &Id) -> Result<Option<Diagnostic>> {
    let expected_account = store.account(account)?;
    let parent = store.root().join("accounts").join(account.as_str());
    // Do not use a directory-creation helper on this read-only path.
    let metadata = std::fs::symlink_metadata(&parent)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || parent.canonicalize()? != parent
    {
        return Err(Error::PrivateState);
    }
    let bytes = match private::read(&parent.join(FILE), MAX_BYTES) {
        Ok(bytes) => bytes,
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let value: Diagnostic = serde_json::from_slice(&bytes)?;
    if value.version != 1
        || &value.account != account
        || value.provider != expected_account.provider
    {
        return Err(Error::PrivateState);
    }
    Ok((&value.request_id == request).then_some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use xcb_core::session::State;

    #[test]
    fn error_projection_is_closed_and_never_copies_error_strings() {
        for (error, category, method, code) in [
            (
                Error::DevinRpc {
                    method: "session/prompt",
                    code: -32011,
                    category: "provider quota or resource limit reached",
                },
                Category::QuotaOrResourceLimit,
                Some(Operation::SessionPrompt),
                Some(-32011),
            ),
            (
                Error::CodexRpc {
                    method: "turn/start",
                    code: 401,
                    category: "authentication rejected; reconnect this account",
                },
                Category::Authentication,
                Some(Operation::TurnStart),
                Some(401),
            ),
            (
                Error::DevinRpc {
                    method: "SECRET /private/path",
                    code: -32603,
                    category: "SECRET token=secret",
                },
                Category::ProviderRejected,
                Some(Operation::Other),
                Some(-32603),
            ),
            (
                Error::Protocol("SECRET /private/path"),
                Category::Protocol,
                None,
                None,
            ),
            (
                Error::DevinModelChoices {
                    shape: "SECRET",
                    count: Some(99999),
                },
                Category::ModelUnavailable,
                None,
                None,
            ),
            (
                Error::Io(std::io::Error::other("SECRET token=secret")),
                Category::LocalIo,
                None,
                None,
            ),
        ] {
            let actual = classify(&error);
            assert_eq!(actual, (category, method, code));
            assert!(!serde_json::to_string(&actual).unwrap().contains("SECRET"));
        }
        assert_eq!(
            protocol_reason("Devin model changed"),
            ProtocolReason::ModelChanged
        );
        assert_eq!(
            protocol_reason("Devin unknown session update"),
            ProtocolReason::UnknownSessionUpdate
        );
        assert_eq!(
            protocol_reason("SECRET /private/path"),
            ProtocolReason::Other
        );
    }

    #[test]
    fn diagnostics_are_private_bounded_request_bound_and_require_owned_run() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().canonicalize().unwrap().join("state")).unwrap();
        let account = store
            .add_account(Provider::Devin, "Synthetic", 1, None)
            .unwrap();
        let first = crate::new_id("application");
        assert!(read(&store, &account.id, &first).unwrap().is_none());
        let run = store.prepare_probe(&account.id, None, now_ms()).unwrap();
        record_error(
            &store,
            &run,
            &first,
            Stage::Start,
            &Error::DevinRpc {
                method: "session/prompt",
                code: -32011,
                category: "provider quota or resource limit reached",
            },
        );
        let value = read(&store, &account.id, &first).unwrap().unwrap();
        assert_eq!(value.category, Category::QuotaOrResourceLimit);
        let path = store.account_root(&account.id).unwrap().join(FILE);
        let bytes = private::read(&path, MAX_BYTES).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains(temp.path().to_str().unwrap()));
        store.settle(&run, State::Failed, now_ms()).unwrap();
        let second = crate::new_id("application");
        record_category(
            &store,
            &run,
            &second,
            Stage::Output,
            Category::UnexpectedEvent,
        );
        assert_eq!(
            private::read(&path, MAX_BYTES).unwrap(),
            bytes,
            "settled ownership must not write"
        );
        let next = store.prepare_probe(&account.id, None, now_ms()).unwrap();
        record_category(
            &store,
            &next,
            &second,
            Stage::Output,
            Category::IncompleteOutput,
        );
        assert!(read(&store, &account.id, &first).unwrap().is_none());
        assert_eq!(
            read(&store, &account.id, &second)
                .unwrap()
                .unwrap()
                .category,
            Category::IncompleteOutput
        );
        assert_eq!(
            store.unsettled_runs().unwrap().len(),
            1,
            "diagnostics cannot settle custody"
        );
        store.settle(&next, State::Failed, now_ms()).unwrap();
    }

    #[test]
    fn diagnostic_reads_reject_wrong_provider_extra_fields_and_nonprivate_parent() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().canonicalize().unwrap().join("state")).unwrap();
        let account = store
            .add_account(Provider::Devin, "Synthetic", 1, None)
            .unwrap();
        let run = store.prepare_probe(&account.id, None, now_ms()).unwrap();
        let id = crate::new_id("application");
        record_error(
            &store,
            &run,
            &id,
            Stage::Initialize,
            &Error::Protocol("Devin model changed"),
        );
        let parent = store.account_root(&account.id).unwrap();
        let path = parent.join(FILE);
        let original = private::read(&path, MAX_BYTES).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&original).unwrap();
        assert_eq!(value["protocolReason"], "model_changed");
        for (field, replacement) in [
            ("provider", "claude"),
            ("prompt", "SECRET"),
            ("category", "SECRET"),
        ] {
            let mut changed = value.clone();
            changed[field] = serde_json::json!(replacement);
            let changed = serde_json::to_vec(&changed).unwrap();
            private::replace(&path, &changed, &digest(&original)).unwrap();
            assert!(read(&store, &account.id, &id).is_err());
            private::replace(&path, &original, &digest(&changed)).unwrap();
        }
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(read(&store, &account.id, &id).is_err());
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(read(&store, &account.id, &id).unwrap().is_some());
        store.settle(&run, State::Failed, now_ms()).unwrap();
    }
}
