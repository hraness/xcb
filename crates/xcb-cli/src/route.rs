//! Machine routing command. A closed stdin document selects one eligible
//! account/model route and runs one bounded turn; all failure output is
//! deliberately sanitized.
use std::{
    io::{self, IsTerminal, Read},
    path::Path,
    sync::Arc,
    time::Duration,
};
use tokio::sync::watch;
use xcb_runtime::{
    Result,
    route::{self, MAX_REQUEST_BYTES, RouteCode, RouteFailure, RouteTaskRequest},
    runner::{Observer, Progress},
    store::Store,
};

fn emit(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

fn failed(code: RouteCode) -> Result<i32> {
    emit(&RouteFailure::unstarted(code))?;
    Ok(1)
}

async fn deadline(timeout_ms: Option<u64>) {
    match timeout_ms {
        Some(ms) => tokio::time::sleep(Duration::from_millis(ms)).await,
        None => std::future::pending().await,
    }
}

pub async fn dispatch(root: &Path, as_json: bool) -> Result<i32> {
    if !as_json || io::stdin().is_terminal() {
        return failed(RouteCode::InvalidRequest);
    }
    let mut bytes = Vec::new();
    if io::stdin()
        .take((MAX_REQUEST_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return failed(RouteCode::InvalidRequest);
    }
    let request = match RouteTaskRequest::parse(&bytes) {
        Ok(request) => request,
        Err(code) => return failed(code),
    };
    let store = match Store::open(root) {
        Ok(store) => Arc::new(store),
        Err(_) => return failed(RouteCode::Unavailable),
    };
    let timeout_ms = request.timeout_ms;
    let (cancel, receiver) = watch::channel(false);
    // A caller deadline or signal cancels the turn; the response is emitted
    // only after the owned process future settles — never by dropping it.
    let mut interrupt =
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()) {
            Ok(value) => value,
            Err(_) => return failed(RouteCode::Unavailable),
        };
    let mut terminate =
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(value) => value,
            Err(_) => return failed(RouteCode::Unavailable),
        };
    let observer: Observer = Arc::new(|event| {
        if let Progress::Notice(message) = event {
            eprintln!("xcb: {message}");
        }
    });
    let task = route::dispatch(store, request, receiver, observer);
    tokio::pin!(task);
    let mut timed_out = false;
    let outcome = tokio::select! {
        result = &mut task => result,
        _ = interrupt.recv() => { let _ = cancel.send(true); task.await },
        _ = terminate.recv() => { let _ = cancel.send(true); task.await },
        _ = deadline(timeout_ms) => { timed_out = true; let _ = cancel.send(true); task.await },
    };
    let outcome = match outcome {
        Err(mut failure) if timed_out && failure.code == RouteCode::Cancelled => {
            failure.code = RouteCode::Deadline;
            Err(failure)
        }
        outcome => outcome,
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
