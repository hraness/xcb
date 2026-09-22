//! Closed machine-only application command. All failure output is deliberately
//! sanitized; provider errors and native stderr never cross this protocol.
use std::{
    io::{self, IsTerminal, Read},
    path::Path,
    sync::Arc,
};
use tokio::sync::watch;
use xcb_runtime::{
    Result,
    application::{self, FailureCode, GenerateFailure, GenerateRequest},
    store::Store,
};

fn emit(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

fn failed(code: FailureCode) -> Result<i32> {
    emit(&GenerateFailure::unstarted(code))?;
    Ok(1)
}

pub async fn dispatch(root: &Path, capabilities: bool, as_json: bool) -> Result<i32> {
    if !as_json {
        return failed(FailureCode::InvalidRequest);
    }
    // Capability discovery must not create a brand-new XCB installation.
    if !root.exists() {
        if capabilities {
            emit(&application::empty_capabilities())?;
            return Ok(0);
        }
        return failed(FailureCode::Unavailable);
    }
    let opened = if capabilities {
        Store::open_read_only(root)
    } else {
        Store::open(root)
    };
    let store = match opened {
        Ok(store) => Arc::new(store),
        Err(_) => return failed(FailureCode::Unavailable),
    };
    if capabilities {
        return match application::capabilities(&store) {
            Ok(value) => {
                emit(&value)?;
                Ok(0)
            }
            Err(_) => failed(FailureCode::Unavailable),
        };
    }
    if io::stdin().is_terminal() {
        return failed(FailureCode::InvalidRequest);
    }
    let mut bytes = Vec::new();
    if io::stdin()
        .take((application::MAX_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return failed(FailureCode::InvalidRequest);
    }
    let request = match GenerateRequest::parse(&bytes) {
        Ok(request) => request,
        Err(code) => return failed(code),
    };
    let (cancel, receiver) = watch::channel(false);
    // Keep the inference future owned until physical join and credential/store
    // settlement. Neither SIGINT nor SIGTERM drops the cleanup future.
    let mut interrupt =
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()) {
            Ok(value) => value,
            Err(_) => return failed(FailureCode::Unavailable),
        };
    let mut terminate =
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(value) => value,
            Err(_) => return failed(FailureCode::Unavailable),
        };
    let task = application::generate(store, request, receiver);
    tokio::pin!(task);
    let outcome = tokio::select! {
        result = &mut task => result,
        _ = interrupt.recv() => { let _ = cancel.send(true); task.await },
        _ = terminate.recv() => { let _ = cancel.send(true); task.await },
    };
    match outcome {
        Ok(value) => {
            emit(&value)?;
            Ok(0)
        }
        Err(value) => {
            emit(&value)?;
            Ok(1)
        }
    }
}

/// Qualification is a separate host command with a fixed challenge, never a
/// field or escape hatch on the application's generate protocol.
pub async fn qualify_dispatch(
    root: &Path,
    account: xcb_core::Id,
    model: String,
    evidence: &Path,
    expected_generation: Option<&str>,
    as_json: bool,
) -> Result<i32> {
    if let Some(expected) = expected_generation
        && application::validate_expected_generation(expected).is_err()
    {
        return failed(FailureCode::InvalidRequest);
    }
    if !as_json || !root.exists() {
        return failed(FailureCode::Unavailable);
    }
    let store = match Store::open(root) {
        Ok(store) => Arc::new(store),
        Err(_) => return failed(FailureCode::Unavailable),
    };
    let (cancel, receiver) = watch::channel(false);
    let mut interrupt =
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()) {
            Ok(value) => value,
            Err(_) => return failed(FailureCode::Unavailable),
        };
    let mut terminate =
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(value) => value,
            Err(_) => return failed(FailureCode::Unavailable),
        };
    let task = application::qualify_with_expected_generation(
        store,
        account,
        model,
        evidence,
        expected_generation,
        receiver,
    );
    tokio::pin!(task);
    let outcome = tokio::select! {
        result = &mut task => result,
        _ = interrupt.recv() => { let _ = cancel.send(true); task.await },
        _ = terminate.recv() => { let _ = cancel.send(true); task.await },
    };
    match outcome {
        Ok(value) => {
            emit(&value)?;
            Ok(0)
        }
        Err(value) => {
            emit(&value)?;
            Ok(1)
        }
    }
}

/// No state initialization, provider launch, refresh, or credential reads.
pub fn inspect_dispatch(
    root: &Path,
    account: &xcb_core::Id,
    model: &str,
    as_json: bool,
) -> Result<i32> {
    if !as_json {
        return failed(FailureCode::InvalidRequest);
    }
    let context = Store::open_read_only(root)
        .and_then(|store| application::qualification_context(&store, account, model));
    match context {
        Ok(value) => {
            emit(&value)?;
            Ok(0)
        }
        Err(_) => failed(FailureCode::Unavailable),
    }
}

/// Read only sanitized metadata; never initialize state or launch a provider.
pub fn diagnostic_dispatch(
    root: &Path,
    account: &xcb_core::Id,
    request: &xcb_core::Id,
    as_json: bool,
) -> Result<i32> {
    if !as_json {
        return failed(FailureCode::InvalidRequest);
    }
    match Store::open_read_only(root)
        .and_then(|store| xcb_runtime::application_diagnostic::read(&store, account, request))
    {
        Ok(Some(value)) => {
            emit(&value)?;
            Ok(0)
        }
        Ok(None) | Err(_) => failed(FailureCode::Unavailable),
    }
}
